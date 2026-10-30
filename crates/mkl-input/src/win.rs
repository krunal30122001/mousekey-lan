//! Windows backend: low-level hooks for capture, `SendInput` for injection, and a hidden
//! window for session-lock, display-change and power notifications.
//!
//! Limitations (by OS design, see docs/PLATFORM.md): hooks and `SendInput` do not reach the
//! secure desktop (lock screen, sign-in, UAC prompts, Ctrl+Alt+Del), and a non-elevated
//! process cannot inject into elevated windows (UIPI).

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{GetLastError, BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, WTSUnRegisterSessionNotification};
use windows_sys::Win32::System::Threading::{GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetSystemMetrics, PostThreadMessageW, RegisterClassW, SetCursorPos, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_QUIT, WNDCLASSW,
};

use crate::{
    CaptureSink, Disposition, HookEvent, InputError, InputEvent, InputInjector, MouseButton, Rect, SystemEvent,
};

/// Tag placed in `dwExtraInfo` of everything we inject (helps when debugging with Spy++).
const MKL_EXTRA_INFO: usize = 0x4D4B_4C4E;

const LLMHF_INJECTED: u32 = 0x01;
const LLKHF_EXTENDED: u32 = 0x01;
const LLKHF_INJECTED: u32 = 0x10;

const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_SYSKEYUP: u32 = 0x0105;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_XBUTTONDOWN: u32 = 0x020B;
const WM_XBUTTONUP: u32 = 0x020C;
const WM_MOUSEHWHEEL: u32 = 0x020E;
const WM_DISPLAYCHANGE: u32 = 0x007E;
const WM_POWERBROADCAST: u32 = 0x0218;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WM_CLIPBOARDUPDATE: u32 = 0x031D;
const WTS_SESSION_LOCK: u32 = 0x7;
const WTS_SESSION_UNLOCK: u32 = 0x8;
const PBT_APMSUSPEND: u32 = 0x4;
const PBT_APMRESUMESUSPEND: u32 = 0x7;
const PBT_APMRESUMEAUTOMATIC: u32 = 0x12;
const NOTIFY_FOR_THIS_SESSION: u32 = 0;
const XBUTTON1: u32 = 0x0001;
const XBUTTON2: u32 = 0x0002;
const MONITORINFOF_PRIMARY: u32 = 0x1;

const WM_TIMER: u32 = 0x0113;
const WATCHDOG_MS: u32 = 2000;

/// `GetTickCount` of the most recent hook callback (any input, injected or not).
static LAST_HOOK_TICK: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
struct LastInputInfo {
    cb_size: u32,
    dw_time: u32,
}

#[link(name = "user32")]
extern "system" {
    fn GetLastInputInfo(plii: *mut LastInputInfo) -> i32;
    fn SetTimer(hwnd: HWND, id: usize, elapse: u32, func: Option<unsafe extern "system" fn(HWND, u32, usize, u32)>) -> usize;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetTickCount() -> u32;
}

/// The session is locked: input goes to the secure Winlogon desktop, which user-session hooks
/// can never see, so a silent hook is expected and not a failure.
static SESSION_LOCKED: AtomicBool = AtomicBool::new(false);

/// Pure watchdog rule (tick values wrap, so compare as signed differences).
fn watchdog_says_dead(locked: bool, last_input_tick: u32, last_hook_tick: u32) -> bool {
    !locked && (last_input_tick.wrapping_sub(last_hook_tick) as i32) > 1000
}

/// Windows silently removes low-level hooks that time out. Detect it: the system saw input
/// (any input updates the last-input time) that our hooks never received.
fn hooks_look_dead() -> bool {
    let mut lii = LastInputInfo { cb_size: size_of::<LastInputInfo>() as u32, dw_time: 0 };
    if unsafe { GetLastInputInfo(&mut lii) } == 0 {
        return false;
    }
    watchdog_says_dead(SESSION_LOCKED.load(Ordering::Relaxed), lii.dw_time, LAST_HOOK_TICK.load(Ordering::Relaxed))
}

thread_local! {
    // Low-level hooks are invoked on the thread that installed them, so the sink can live in
    // thread-local storage and be read without locking.
    static SINK: RefCell<Option<Arc<dyn CaptureSink>>> = const { RefCell::new(None) };
}

/// Makes this process per-monitor DPI aware (v2) so every coordinate is in physical pixels.
/// Safe to call more than once; later calls are ignored by the OS.
pub fn init_process_dpi() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Owns the hook thread. Dropping it removes the hooks.
pub struct CaptureHandle {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn start_capture(sink: Arc<dyn CaptureSink>) -> Result<CaptureHandle, InputError> {
    let (tx, rx) = mpsc::channel::<Result<u32, InputError>>();
    let join = std::thread::Builder::new()
        .name("mkl-input-hook".into())
        .spawn(move || unsafe { hook_thread(sink, tx) })
        .map_err(|e| InputError::Thread(e.to_string()))?;
    match rx.recv() {
        Ok(Ok(thread_id)) => Ok(CaptureHandle { thread_id, join: Some(join) }),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => Err(InputError::Thread("hook thread exited during start-up".into())),
    }
}

unsafe fn hook_thread(sink: Arc<dyn CaptureSink>, ready: mpsc::Sender<Result<u32, InputError>>) {
    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    // All input on the PC waits for this thread, and Windows removes hooks that answer late.
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
    SINK.with(|s| *s.borrow_mut() = Some(sink));

    let hmod = GetModuleHandleW(null());
    let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0);
    let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hmod, 0);
    if mouse.is_null() || keyboard.is_null() {
        let err = GetLastError();
        if !mouse.is_null() {
            UnhookWindowsHookEx(mouse);
        }
        if !keyboard.is_null() {
            UnhookWindowsHookEx(keyboard);
        }
        let _ = ready.send(Err(InputError::HookInstall(err)));
        return;
    }

    let hwnd = create_notification_window(hmod);
    if !hwnd.is_null() {
        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        crate::win_desktop::init_on_hook_thread(hmod, hwnd);
    } else {
        tracing::warn!("could not create notification window; lock/display events unavailable");
    }

    let _ = ready.send(Ok(GetCurrentThreadId()));
    tracing::info!("input hooks installed");

    let (mut mouse, mut keyboard) = (mouse, keyboard);
    LAST_HOOK_TICK.store(GetTickCount(), Ordering::Relaxed);
    SetTimer(null_mut(), 0, WATCHDOG_MS, None);
    let mut msg: MSG = zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        if msg.message == WM_TIMER && msg.hwnd.is_null() {
            if hooks_look_dead() {
                tracing::warn!("Windows removed the input hooks (timeout); reinstalling them");
                UnhookWindowsHookEx(mouse);
                UnhookWindowsHookEx(keyboard);
                mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0);
                keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hmod, 0);
                LAST_HOOK_TICK.store(GetTickCount(), Ordering::Relaxed);
            }
            continue;
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    if !hwnd.is_null() {
        crate::win_desktop::shutdown_on_hook_thread();
        WTSUnRegisterSessionNotification(hwnd);
        DestroyWindow(hwnd);
    }
    UnhookWindowsHookEx(mouse);
    UnhookWindowsHookEx(keyboard);
    SINK.with(|s| *s.borrow_mut() = None);
    tracing::info!("input hooks removed");
}

/// Hidden top-level window (message-only windows do not receive broadcast messages such as
/// `WM_DISPLAYCHANGE` and `WM_POWERBROADCAST`).
unsafe fn create_notification_window(hmod: *mut core::ffi::c_void) -> HWND {
    let class: Vec<u16> = "MouseKeyLanNotify\0".encode_utf16().collect();
    let mut wc: WNDCLASSW = zeroed();
    wc.lpfnWndProc = Some(wnd_proc);
    wc.hInstance = hmod;
    wc.lpszClassName = class.as_ptr();
    RegisterClassW(&wc);
    CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), hmod, null())
}

fn current_sink() -> Option<Arc<dyn CaptureSink>> {
    SINK.with(|s| s.borrow().clone())
}

fn dispatch_input(ev: HookEvent) -> bool {
    let Some(sink) = current_sink() else { return false };
    matches!(catch_unwind(AssertUnwindSafe(|| sink.on_input(ev))), Ok(Disposition::Swallow))
}

/// Files dragged onto the drop catcher (called on the hook thread by the OLE drop target).
pub(crate) fn dispatch_drag_files(files: Vec<std::path::PathBuf>) {
    if let Some(sink) = current_sink() {
        let _ = catch_unwind(AssertUnwindSafe(|| sink.on_drag_files(files)));
    }
}

/// A drag was released on the drop catcher.
pub(crate) fn dispatch_drag_dropped() {
    if let Some(sink) = current_sink() {
        let _ = catch_unwind(AssertUnwindSafe(|| sink.on_drag_dropped()));
    }
}

fn dispatch_system(ev: SystemEvent) {
    if let Some(sink) = current_sink() {
        let _ = catch_unwind(AssertUnwindSafe(|| sink.on_system(ev)));
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    LAST_HOOK_TICK.store(GetTickCount(), Ordering::Relaxed);
    if code == 0 {
        let info = &*(lparam as *const MSLLHOOKSTRUCT);
        if info.flags & LLMHF_INJECTED == 0 {
            if let Some(ev) = translate_mouse(wparam as u32, info) {
                if dispatch_input(ev) {
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    LAST_HOOK_TICK.store(GetTickCount(), Ordering::Relaxed);
    if code == 0 {
        let info = &*(lparam as *const KBDLLHOOKSTRUCT);
        if info.flags & LLKHF_INJECTED == 0 {
            let down = match wparam as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => Some(true),
                WM_KEYUP | WM_SYSKEYUP => Some(false),
                _ => None,
            };
            if let Some(down) = down {
                let ev = HookEvent::Key {
                    vk: info.vkCode as u16,
                    scan: info.scanCode as u16,
                    extended: info.flags & LLKHF_EXTENDED != 0,
                    down,
                };
                if dispatch_input(ev) {
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

fn translate_mouse(msg: u32, info: &MSLLHOOKSTRUCT) -> Option<HookEvent> {
    let hi = ((info.mouseData as u32) >> 16) as u16;
    let button = |button, down| Some(HookEvent::Button { button, down });
    match msg {
        WM_MOUSEMOVE => Some(HookEvent::MouseMove { x: info.pt.x, y: info.pt.y }),
        WM_LBUTTONDOWN => button(MouseButton::Left, true),
        WM_LBUTTONUP => button(MouseButton::Left, false),
        WM_RBUTTONDOWN => button(MouseButton::Right, true),
        WM_RBUTTONUP => button(MouseButton::Right, false),
        WM_MBUTTONDOWN => button(MouseButton::Middle, true),
        WM_MBUTTONUP => button(MouseButton::Middle, false),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let b = if hi as u32 == XBUTTON2 { MouseButton::X2 } else { MouseButton::X1 };
            button(b, msg == WM_XBUTTONDOWN)
        }
        WM_MOUSEWHEEL => Some(HookEvent::Wheel { delta: hi as i16 as i32, horizontal: false }),
        WM_MOUSEHWHEEL => Some(HookEvent::Wheel { delta: hi as i16 as i32, horizontal: true }),
        _ => None,
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ev = match msg {
        WM_WTSSESSION_CHANGE => match wparam as u32 {
            WTS_SESSION_LOCK => Some(SystemEvent::SessionLocked),
            WTS_SESSION_UNLOCK => Some(SystemEvent::SessionUnlocked),
            _ => None,
        },
        WM_DISPLAYCHANGE => Some(SystemEvent::DisplayChanged),
        WM_CLIPBOARDUPDATE => Some(SystemEvent::ClipboardChanged),
        WM_POWERBROADCAST => match wparam as u32 {
            PBT_APMSUSPEND => Some(SystemEvent::Suspend),
            PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => Some(SystemEvent::Resume),
            _ => None,
        },
        _ => None,
    };
    if let Some(ev) = ev {
        match ev {
            SystemEvent::SessionLocked => SESSION_LOCKED.store(true, Ordering::Relaxed),
            SystemEvent::SessionUnlocked | SystemEvent::Resume => {
                // Lock-screen typing counted as "system input"; restart the comparison.
                SESSION_LOCKED.store(false, Ordering::Relaxed);
                LAST_HOOK_TICK.store(GetTickCount(), Ordering::Relaxed);
            }
            _ => {}
        }
        dispatch_system(ev);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// `SendInput`-based injector.
#[derive(Default)]
pub struct WinInjector;

impl WinInjector {
    fn send(&self, input: INPUT) -> Result<(), InputError> {
        let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
        if sent == 1 {
            Ok(())
        } else {
            Err(InputError::Inject(unsafe { GetLastError() }))
        }
    }
}

fn mouse(dx: i32, dy: i32, data: i32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as _,
                dwFlags: flags as _,
                time: 0,
                dwExtraInfo: MKL_EXTRA_INFO,
            },
        },
    }
}

/// Maps a virtual-desktop pixel to the 0..=65535 range used by `MOUSEEVENTF_ABSOLUTE`.
/// Rounds up so that Windows' truncating inverse mapping lands on exactly `pixel`.
fn normalize(pixel: i32, origin: i32, extent: i32) -> i32 {
    let extent = extent.max(1) as i64;
    let offset = (pixel - origin).clamp(0, extent as i32 - 1) as i64;
    ((offset * 65_536 + extent - 1) / extent) as i32
}

impl InputInjector for WinInjector {
    fn inject(&self, ev: &InputEvent) -> Result<(), InputError> {
        let input = match *ev {
            InputEvent::MouseMove { x, y } => {
                let (vx, vy, vw, vh) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN),
                    )
                };
                mouse(
                    normalize(x, vx, vw),
                    normalize(y, vy, vh),
                    0,
                    (MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK) as u32,
                )
            }
            InputEvent::Button { button, down } => {
                let (flags, data) = match (button, down) {
                    (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (MouseButton::X1, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                    (MouseButton::X1, false) => (MOUSEEVENTF_XUP, XBUTTON1),
                    (MouseButton::X2, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                    (MouseButton::X2, false) => (MOUSEEVENTF_XUP, XBUTTON2),
                };
                mouse(0, 0, data as i32, flags as u32)
            }
            InputEvent::Wheel { delta, horizontal } => {
                let flags = if horizontal { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL };
                mouse(0, 0, delta, flags as u32)
            }
            InputEvent::Key { vk, scan, extended, down } => {
                let mut flags = 0u32;
                let mut w_vk = 0u16;
                if scan != 0 {
                    flags |= KEYEVENTF_SCANCODE as u32;
                } else {
                    w_vk = vk;
                }
                if extended {
                    flags |= KEYEVENTF_EXTENDEDKEY as u32;
                }
                if !down {
                    flags |= KEYEVENTF_KEYUP as u32;
                }
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: w_vk,
                            wScan: scan,
                            dwFlags: flags as _,
                            time: 0,
                            dwExtraInfo: MKL_EXTRA_INFO,
                        },
                    },
                }
            }
        };
        self.send(input)
    }

    fn screens(&self) -> Vec<Rect> {
        unsafe extern "system" fn collect(hmon: HMONITOR, _hdc: HDC, _rc: *mut RECT, data: LPARAM) -> BOOL {
            let out = &mut *(data as *mut Vec<(Rect, bool)>);
            let mut mi: MONITORINFO = zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(hmon, &mut mi) != 0 {
                let r = mi.rcMonitor;
                out.push((
                    Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top),
                    mi.dwFlags & MONITORINFOF_PRIMARY != 0,
                ));
            }
            1
        }
        let mut found: Vec<(Rect, bool)> = Vec::new();
        unsafe {
            EnumDisplayMonitors(null_mut(), null(), Some(collect), &mut found as *mut _ as LPARAM);
        }
        found.sort_by_key(|(r, primary)| (!*primary, r.x, r.y));
        let screens: Vec<Rect> = found.into_iter().map(|(r, _)| r).filter(|r| r.w > 0 && r.h > 0).collect();
        if screens.is_empty() {
            let (w, h) = unsafe { (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) };
            vec![Rect::new(0, 0, w.max(1), h.max(1))]
        } else {
            screens
        }
    }

    fn set_cursor_pos(&self, x: i32, y: i32) {
        unsafe {
            SetCursorPos(x, y);
        }
    }

    fn cursor_pos(&self) -> (i32, i32) {
        let mut p = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut p);
        }
        (p.x, p.y)
    }

    fn is_key_down(&self, vk: u16) -> bool {
        unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_hits_exact_pixels() {
        for &(origin, extent) in &[(0, 1920), (-1920, 4480), (0, 7680), (-3840, 3841)] {
            for px in [origin, origin + 1, origin + extent / 2, origin + extent - 1] {
                let n = normalize(px, origin, extent) as i64;
                assert!(n <= 65_535);
                let back = origin as i64 + n * extent as i64 / 65_536;
                assert_eq!(back, px as i64, "origin {origin} extent {extent} px {px}");
            }
        }
    }

    #[test]
    fn enumerates_at_least_one_screen() {
        assert!(!WinInjector.screens().is_empty());
    }
}

#[cfg(test)]
mod watchdog_tests {
    use super::watchdog_says_dead;

    #[test]
    fn detects_a_silent_hook_only_while_unlocked() {
        // Input at tick 10_000 but the hook last ran at 5_000: dead while unlocked...
        assert!(watchdog_says_dead(false, 10_000, 5_000));
        // ...but expected while the session is locked (lock-screen typing).
        assert!(!watchdog_says_dead(true, 10_000, 5_000));
        // Healthy: hook ran at (or after) the last input.
        assert!(!watchdog_says_dead(false, 10_000, 10_000));
        assert!(!watchdog_says_dead(false, 10_000, 10_500));
        // Small gaps are normal.
        assert!(!watchdog_says_dead(false, 10_900, 10_000));
    }

    #[test]
    fn survives_tick_counter_wraparound() {
        assert!(!watchdog_says_dead(false, 100, u32::MAX - 50)); // 151 ms apart
        assert!(watchdog_says_dead(false, 5_000, u32::MAX - 500)); // ~5.5 s apart
    }
}
