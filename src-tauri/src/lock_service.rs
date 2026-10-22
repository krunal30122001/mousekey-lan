//! Lock-screen input helper management.
//!
//! Instead of a Windows Service (Session 0 — cannot reach user desktop), we
//! register a scheduled task that runs `mkl-service.exe` at every logon in
//! the INTERACTIVE user session (/IT flag). Combined with a `uiAccess="true"`
//! manifest and installation in `Program Files\MouseKey LAN` (a trusted path),
//! the helper can call `SendInput` that reaches the Windows lock screen /
//! Winlogon secure desktop.
//!
//! This is the exact same mechanism used by the "Run as administrator" feature
//! (see elevation.rs).

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::ptr::{null, null_mut};

const TASK_NAME: &str = "MouseKey LAN Lock Helper";
const INSTALL_DIR: &str = r"C:\Program Files\MouseKey LAN";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
const SW_HIDE: i32 = 0;
const INFINITE: u32 = 0xFFFF_FFFF;

#[repr(C)]
struct ShellExecuteInfoW {
    cb_size: u32,
    f_mask: u32,
    hwnd: *mut c_void,
    verb: *const u16,
    file: *const u16,
    parameters: *const u16,
    directory: *const u16,
    show: i32,
    inst_app: *mut c_void,
    id_list: *mut c_void,
    class: *const u16,
    hkey_class: *mut c_void,
    hot_key: u32,
    icon_or_monitor: *mut c_void,
    process: *mut c_void,
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
    fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
    fn CloseHandle(h: *mut c_void) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Run `cmd.exe /c <script>` elevated (UAC prompt) and return exit code.
/// Returns Err if the user cancelled the UAC dialog.
fn run_elevated(script: &str) -> Result<u32, String> {
    let verb = wide("runas");
    let file = wide("cmd.exe");
    let params = wide(&format!("/c {script}"));
    let mut info = ShellExecuteInfoW {
        cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
        f_mask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: null_mut(),
        verb: verb.as_ptr(),
        file: file.as_ptr(),
        parameters: params.as_ptr(),
        directory: null(),
        show: SW_HIDE,
        inst_app: null_mut(),
        id_list: null_mut(),
        class: null(),
        hkey_class: null_mut(),
        hot_key: 0,
        icon_or_monitor: null_mut(),
        process: null_mut(),
    };
    unsafe {
        if ShellExecuteExW(&mut info) == 0 {
            return Err("Administrator permission was cancelled.".into());
        }
        if info.process.is_null() {
            return Ok(0);
        }
        WaitForSingleObject(info.process, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(info.process, &mut code);
        CloseHandle(info.process);
        Ok(code)
    }
}

/// True if the scheduled task exists (helper is installed).
pub fn is_installed() -> bool {
    std::process::Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// True if mkl-service.exe process is running in the user's session.
pub fn is_running() -> bool {
    let out = std::process::Command::new("tasklist.exe")
        .args(["/FI", "IMAGENAME eq mkl-service.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap_or_else(|_| std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: vec![],
            stderr: vec![],
        });
    let text = String::from_utf8_lossy(&out.stdout);
    text.contains("mkl-service.exe")
}

/// Find mkl-service.exe next to the main exe or in Program Files.
fn find_service_exe() -> Result<std::path::PathBuf, String> {
    if let Ok(mut p) = std::env::current_exe() {
        p.set_file_name("mkl-service.exe");
        if p.exists() {
            return Ok(p);
        }
    }
    let installed = std::path::Path::new(INSTALL_DIR).join("mkl-service.exe");
    if installed.exists() {
        return Ok(installed);
    }
    Err(format!(
        "mkl-service.exe not found next to MouseKey LAN.exe or in {INSTALL_DIR}"
    ))
}

/// Install: copy exe to Program Files, embed UIAccess manifest, register
/// scheduled task (ONLOGON/IT), then run it immediately.
pub fn install() -> Result<(), String> {
    let src = find_service_exe()?;
    let src_str = src.display().to_string();
    let dst = format!(r"{INSTALL_DIR}\mkl-service.exe");
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );

    // One elevated cmd.exe does everything:
    // 1. mkdir Program Files\MouseKey LAN
    // 2. Copy exe there
    // 3. Delete old task if any
    // 4. Create ONLOGON scheduled task with /IT (interactive) so it runs in
    //    the user's session — NOT Session 0
    // 5. Run it immediately
    let script = format!(
        r#"mkdir "{INSTALL_DIR}" 2>nul & copy /Y "{src_str}" "{dst}" && schtasks /Delete /F /TN "{TASK_NAME}" 2>nul & schtasks /Create /F /TN "{TASK_NAME}" /SC ONLOGON /IT /RL HIGHEST /RU "{user}" /TR "\"{dst}\"" && schtasks /Run /TN "{TASK_NAME}""#
    );

    let code = run_elevated(&script)?;
    if code != 0 {
        return Err(format!("Lock-screen helper installation failed (exit {code})."));
    }
    Ok(())
}

/// Uninstall: kill the process and delete the scheduled task.
pub fn uninstall() -> Result<(), String> {
    let script = format!(
        r#"taskkill /F /IM mkl-service.exe 2>nul & schtasks /Delete /F /TN "{TASK_NAME}""#
    );
    let code = run_elevated(&script)?;
    if code != 0 {
        return Err(format!("Lock-screen helper removal failed (exit {code})."));
    }
    Ok(())
}

// ── Public surface used by commands.rs ──────────────────────────────────────

pub fn service_installed() -> bool {
    is_installed()
}

pub fn service_running() -> bool {
    is_running()
}
