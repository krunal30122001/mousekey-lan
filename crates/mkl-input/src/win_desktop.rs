//! Windows clipboard access and drag detection.
//!
//! * Clipboard: `CF_UNICODETEXT`, `CF_DIB` and `CF_HDROP` (file lists), owned by the hidden
//!   notification window of the hook thread (a window is required for `SetClipboardData`).
//! * Drag detection: a tiny, nearly transparent, topmost window registered as an OLE drop
//!   target. When the user drags files against a screen edge that leads to another PC, the
//!   router shows this window under the cursor; the drag source then reports the dragged
//!   files to it (`DragEnter`), which is how they are carried to the other PC.

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::Duration;

use crate::{ClipContent, Desktop, InputError};

type Hwnd = *mut c_void;
type Handle = *mut c_void;
type HResult = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct PointL {
    x: i32,
    y: i32,
}

#[repr(C)]
struct FormatEtc {
    cf_format: u16,
    ptd: *mut c_void,
    dw_aspect: u32,
    lindex: i32,
    tymed: u32,
}

#[repr(C)]
struct StgMedium {
    tymed: u32,
    handle: *mut c_void,
    unk_for_release: *mut c_void,
}

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(hwnd: Hwnd) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> Handle;
    fn SetClipboardData(format: u32, mem: Handle) -> Handle;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn GetClipboardSequenceNumber() -> u32;
    fn AddClipboardFormatListener(hwnd: Hwnd) -> i32;
    fn RemoveClipboardFormatListener(hwnd: Hwnd) -> i32;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
    fn SetLayeredWindowAttributes(hwnd: Hwnd, key: u32, alpha: u8, flags: u32) -> i32;
    fn SetWindowPos(hwnd: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Hwnd,
        menu: *mut c_void,
        instance: *mut c_void,
        param: *const c_void,
    ) -> Hwnd;
    fn DestroyWindow(hwnd: Hwnd) -> i32;
    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
    fn GlobalLock(mem: Handle) -> *mut c_void;
    fn GlobalUnlock(mem: Handle) -> i32;
    fn GlobalSize(mem: Handle) -> usize;
    fn GlobalFree(mem: Handle) -> Handle;
}

#[link(name = "shell32")]
extern "system" {
    fn DragQueryFileW(hdrop: Handle, index: u32, file: *mut u16, cch: u32) -> u32;
}

#[link(name = "ole32")]
extern "system" {
    fn OleInitialize(reserved: *mut c_void) -> HResult;
    fn OleUninitialize();
    fn RegisterDragDrop(hwnd: Hwnd, target: *mut c_void) -> HResult;
    fn RevokeDragDrop(hwnd: Hwnd) -> HResult;
    fn ReleaseStgMedium(medium: *mut StgMedium);
}

#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: Option<unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: *mut c_void,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: *const u16,
    class_name: *const u16,
}

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const GMEM_MOVEABLE: u32 = 0x0002;
const TYMED_HGLOBAL: u32 = 1;
const DVASPECT_CONTENT: u32 = 1;
const DROPEFFECT_NONE: u32 = 0;
const DROPEFFECT_COPY: u32 = 1;
const S_OK: HResult = 0;
const E_NOINTERFACE: HResult = 0x8000_4002_u32 as i32;
const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_LAYERED: u32 = 0x0008_0000;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const LWA_ALPHA: u32 = 0x2;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_SHOWWINDOW: u32 = 0x0040;
const SWP_HIDEWINDOW: u32 = 0x0080;
const HWND_TOPMOST: Hwnd = -1isize as Hwnd;
const CATCHER_SIZE: i32 = 64;

/// Hidden notification window of the hook thread (clipboard owner).
static NOTIFY_HWND: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
/// Drop catcher window (created on the hook thread).
static CATCHER_HWND: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------------------
// Hook-thread setup / teardown
// ---------------------------------------------------------------------------------------

/// Called on the hook thread after its notification window exists.
pub(crate) unsafe fn init_on_hook_thread(hmod: *mut c_void, notify: Hwnd) {
    NOTIFY_HWND.store(notify, Ordering::Release);
    if AddClipboardFormatListener(notify) == 0 {
        tracing::warn!("clipboard change notifications unavailable");
    }

    OleInitialize(null_mut());
    let class = wide("MouseKeyLanDropCatcher");
    let wc = WndClassW {
        style: 0,
        wnd_proc: Some(DefWindowProcW),
        cls_extra: 0,
        wnd_extra: 0,
        instance: hmod,
        icon: null_mut(),
        cursor: null_mut(),
        background: null_mut(),
        menu_name: null(),
        class_name: class.as_ptr(),
    };
    RegisterClassW(&wc);
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        class.as_ptr(),
        class.as_ptr(),
        WS_POPUP,
        0,
        0,
        CATCHER_SIZE,
        CATCHER_SIZE,
        null_mut(),
        null_mut(),
        hmod,
        null(),
    );
    if hwnd.is_null() {
        tracing::warn!("drag-and-drop detection unavailable (window creation failed)");
        return;
    }
    // Alpha 1/255: invisible to the eye but still a hit-test (and therefore drop) target.
    SetLayeredWindowAttributes(hwnd, 0, 1, LWA_ALPHA);
    let hr = RegisterDragDrop(hwnd, &DROP_TARGET as *const DropTarget as *mut c_void);
    if hr != S_OK {
        tracing::warn!("drag-and-drop detection unavailable (RegisterDragDrop {hr:#x})");
        DestroyWindow(hwnd);
        return;
    }
    CATCHER_HWND.store(hwnd, Ordering::Release);
}

pub(crate) unsafe fn shutdown_on_hook_thread() {
    let notify = NOTIFY_HWND.swap(null_mut(), Ordering::AcqRel);
    if !notify.is_null() {
        RemoveClipboardFormatListener(notify);
    }
    let catcher = CATCHER_HWND.swap(null_mut(), Ordering::AcqRel);
    if !catcher.is_null() {
        RevokeDragDrop(catcher);
        DestroyWindow(catcher);
    }
    OleUninitialize();
}

// ---------------------------------------------------------------------------------------
// Minimal IDropTarget implementation (static object, no reference counting needed)
// ---------------------------------------------------------------------------------------

#[repr(C)]
struct DropTargetVtbl {
    query_interface: unsafe extern "system" fn(*mut DropTarget, *const Guid, *mut *mut c_void) -> HResult,
    add_ref: unsafe extern "system" fn(*mut DropTarget) -> u32,
    release: unsafe extern "system" fn(*mut DropTarget) -> u32,
    drag_enter: unsafe extern "system" fn(*mut DropTarget, *mut c_void, u32, PointL, *mut u32) -> HResult,
    drag_over: unsafe extern "system" fn(*mut DropTarget, u32, PointL, *mut u32) -> HResult,
    drag_leave: unsafe extern "system" fn(*mut DropTarget) -> HResult,
    drop: unsafe extern "system" fn(*mut DropTarget, *mut c_void, u32, PointL, *mut u32) -> HResult,
}

#[repr(C)]
struct DropTarget {
    vtbl: *const DropTargetVtbl,
}

// The object is immutable and only ever used on the hook thread.
unsafe impl Sync for DropTarget {}

static DROP_TARGET_VTBL: DropTargetVtbl = DropTargetVtbl {
    query_interface: dt_query_interface,
    add_ref: dt_add_ref,
    release: dt_release,
    drag_enter: dt_drag_enter,
    drag_over: dt_drag_over,
    drag_leave: dt_drag_leave,
    drop: dt_drop,
};

static DROP_TARGET: DropTarget = DropTarget { vtbl: &DROP_TARGET_VTBL };

fn is_iid(g: &Guid, data1: u32) -> bool {
    g.data1 == data1 && g.data2 == 0 && g.data3 == 0 && g.data4 == [0xC0, 0, 0, 0, 0, 0, 0, 0x46]
}

unsafe extern "system" fn dt_query_interface(this: *mut DropTarget, iid: *const Guid, out: *mut *mut c_void) -> HResult {
    // IUnknown = 00000000-…, IDropTarget = 00000122-…
    if !iid.is_null() && (is_iid(&*iid, 0) || is_iid(&*iid, 0x122)) {
        *out = this as *mut c_void;
        S_OK
    } else {
        *out = null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn dt_add_ref(_this: *mut DropTarget) -> u32 {
    2
}

unsafe extern "system" fn dt_release(_this: *mut DropTarget) -> u32 {
    1
}

unsafe extern "system" fn dt_drag_enter(_this: *mut DropTarget, data: *mut c_void, _keys: u32, _pt: PointL, effect: *mut u32) -> HResult {
    let files = if data.is_null() { Vec::new() } else { files_from_data_object(data) };
    if !effect.is_null() {
        *effect = if files.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY };
    }
    if !files.is_empty() {
        crate::win::dispatch_drag_files(files);
    }
    S_OK
}

unsafe extern "system" fn dt_drag_over(_this: *mut DropTarget, _keys: u32, _pt: PointL, effect: *mut u32) -> HResult {
    if !effect.is_null() {
        *effect = DROPEFFECT_COPY;
    }
    S_OK
}

unsafe extern "system" fn dt_drag_leave(_this: *mut DropTarget) -> HResult {
    S_OK
}

unsafe extern "system" fn dt_drop(_this: *mut DropTarget, _data: *mut c_void, _keys: u32, _pt: PointL, effect: *mut u32) -> HResult {
    // Tell the source nothing was dropped here; the engine decides whether the files go to
    // another PC (a drag that started on this PC while another PC was controlling it).
    crate::win::dispatch_drag_dropped();
    if !effect.is_null() {
        *effect = DROPEFFECT_NONE;
    }
    S_OK
}

/// Calls `IDataObject::GetData(CF_HDROP)` through the COM vtable.
unsafe fn files_from_data_object(data: *mut c_void) -> Vec<PathBuf> {
    type GetData = unsafe extern "system" fn(*mut c_void, *const FormatEtc, *mut StgMedium) -> HResult;
    let vtbl = *(data as *const *const usize);
    let get_data: GetData = std::mem::transmute(*vtbl.add(3));
    let fmt = FormatEtc { cf_format: CF_HDROP as u16, ptd: null_mut(), dw_aspect: DVASPECT_CONTENT, lindex: -1, tymed: TYMED_HGLOBAL };
    let mut medium = StgMedium { tymed: 0, handle: null_mut(), unk_for_release: null_mut() };
    if get_data(data, &fmt, &mut medium) < 0 || medium.handle.is_null() {
        return Vec::new();
    }
    let files = query_drop_files(medium.handle);
    ReleaseStgMedium(&mut medium);
    files
}

unsafe fn query_drop_files(hdrop: Handle) -> Vec<PathBuf> {
    let count = DragQueryFileW(hdrop, u32::MAX, null_mut(), 0);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count.min(10_000) {
        let len = DragQueryFileW(hdrop, i, null_mut(), 0) as usize;
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len + 1];
        let n = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), (len + 1) as u32) as usize;
        out.push(PathBuf::from(OsString::from_wide(&buf[..n])));
    }
    out
}

// ---------------------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------------------

struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

/// Opens the clipboard, retrying briefly while another app holds it.
fn open_clipboard() -> Option<ClipboardGuard> {
    let owner = NOTIFY_HWND.load(Ordering::Acquire);
    for _ in 0..25 {
        if unsafe { OpenClipboard(owner) } != 0 {
            return Some(ClipboardGuard);
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    None
}

unsafe fn global_bytes(h: Handle) -> Option<Vec<u8>> {
    if h.is_null() {
        return None;
    }
    let p = GlobalLock(h) as *const u8;
    if p.is_null() {
        return None;
    }
    let v = std::slice::from_raw_parts(p, GlobalSize(h)).to_vec();
    GlobalUnlock(h);
    Some(v)
}

unsafe fn global_from(bytes: &[u8]) -> Result<Handle, InputError> {
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
    if h.is_null() {
        return Err(InputError::Thread("out of memory for clipboard data".into()));
    }
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        GlobalFree(h);
        return Err(InputError::Thread("could not lock clipboard memory".into()));
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    GlobalUnlock(h);
    Ok(h)
}

unsafe fn set_data(format: u32, bytes: &[u8]) -> Result<(), InputError> {
    let h = global_from(bytes)?;
    if SetClipboardData(format, h).is_null() {
        GlobalFree(h);
        return Err(InputError::Thread("SetClipboardData failed".into()));
    }
    Ok(())
}

/// `DROPFILES` header followed by a double-NUL-terminated list of wide paths.
fn hdrop_bytes(files: &[PathBuf]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&20u32.to_le_bytes()); // pFiles
    v.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    v.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    v.extend_from_slice(&0i32.to_le_bytes()); // fNC
    v.extend_from_slice(&1i32.to_le_bytes()); // fWide
    for f in files {
        for u in f.as_os_str().encode_wide() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v.extend_from_slice(&[0, 0]);
    }
    v.extend_from_slice(&[0, 0]);
    v
}

#[derive(Default)]
pub struct WinDesktop;

impl Desktop for WinDesktop {
    fn clipboard_sequence(&self) -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    fn read_clipboard(&self) -> Option<ClipContent> {
        let _guard = open_clipboard()?;
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) != 0 {
                let files = query_drop_files(GetClipboardData(CF_HDROP));
                if !files.is_empty() {
                    return Some(ClipContent::Files(files));
                }
            }
            if IsClipboardFormatAvailable(CF_DIB) != 0 {
                if let Some(b) = global_bytes(GetClipboardData(CF_DIB)) {
                    return Some(ClipContent::Image(b));
                }
            }
            if IsClipboardFormatAvailable(CF_UNICODETEXT) != 0 {
                if let Some(b) = global_bytes(GetClipboardData(CF_UNICODETEXT)) {
                    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
                    return Some(ClipContent::Text(String::from_utf16_lossy(&units)));
                }
            }
        }
        None
    }

    fn write_clipboard(&self, content: &ClipContent) -> Result<(), InputError> {
        if NOTIFY_HWND.load(Ordering::Acquire).is_null() {
            return Err(InputError::Thread("clipboard owner window not ready".into()));
        }
        let _guard = open_clipboard().ok_or_else(|| InputError::Thread("clipboard is busy".into()))?;
        unsafe {
            EmptyClipboard();
            match content {
                ClipContent::Text(s) => {
                    let w: Vec<u8> = s.encode_utf16().chain(std::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect();
                    set_data(CF_UNICODETEXT, &w)
                }
                ClipContent::Image(dib) => set_data(CF_DIB, dib),
                ClipContent::Files(files) => {
                    set_data(CF_HDROP, &hdrop_bytes(files))?;
                    let fmt = RegisterClipboardFormatW(wide("Preferred DropEffect").as_ptr());
                    if fmt != 0 {
                        let _ = set_data(fmt, &DROPEFFECT_COPY.to_le_bytes());
                    }
                    Ok(())
                }
            }
        }
    }

    fn show_drop_catcher(&self, x: i32, y: i32) {
        let h = CATCHER_HWND.load(Ordering::Acquire);
        if !h.is_null() {
            unsafe {
                SetWindowPos(
                    h,
                    HWND_TOPMOST,
                    x - CATCHER_SIZE / 2,
                    y - CATCHER_SIZE / 2,
                    CATCHER_SIZE,
                    CATCHER_SIZE,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }

    fn hide_drop_catcher(&self) {
        let h = CATCHER_HWND.load(Ordering::Acquire);
        if !h.is_null() {
            unsafe {
                SetWindowPos(h, null_mut(), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE);
            }
        }
    }

    fn reveal(&self, path: &Path) {
        let _ = std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }

    fn is_elevated(&self) -> bool {
        unsafe { process_elevated(GetCurrentProcess()).unwrap_or(false) }
    }

    fn foreground_is_elevated(&self) -> bool {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == 0 {
                return false;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return false;
            }
            let elevated = process_elevated(process).unwrap_or(false);
            CloseHandle(process);
            elevated
        }
    }
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_ELEVATION_CLASS: u32 = 20;

#[link(name = "user32")]
extern "system" {
    fn GetForegroundWindow() -> Hwnd;
    fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn CloseHandle(h: Handle) -> i32;
    fn GetCurrentProcess() -> Handle;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(token: Handle, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
}

unsafe fn process_elevated(process: Handle) -> Option<bool> {
    let mut token: Handle = null_mut();
    if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
        return None;
    }
    let mut elevated: u32 = 0;
    let mut ret = 0u32;
    let ok = GetTokenInformation(token, TOKEN_ELEVATION_CLASS, &mut elevated as *mut u32 as *mut c_void, 4, &mut ret);
    CloseHandle(token);
    (ok != 0).then_some(elevated != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdrop_layout() {
        let b = hdrop_bytes(&[PathBuf::from("C:\\a"), PathBuf::from("D:\\b")]);
        assert_eq!(&b[..4], &20u32.to_le_bytes());
        assert_eq!(b.len(), 20 + (5 * 2) * 2 + 2);
        assert_eq!(&b[b.len() - 4..], &[0, 0, 0, 0]);
    }
}
