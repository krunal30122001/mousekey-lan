//! Lock-screen service management (MouseKeyLANService).
//!
//! `mkl-service.exe` must live next to the main executable (or in `Program Files\MouseKey LAN`).
//! Toggling the service ON triggers a UAC prompt exactly once; all heavy work (sc create,
//! sc start) runs inside that elevated cmd.exe session.

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::ptr::{null, null_mut};

const SERVICE_NAME: &str = "MouseKeyLANService";
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

/// Run `cmd.exe /c <command>` elevated (UAC prompt) and wait for it.
/// Returns Ok(exit_code) or Err if the user declined.
fn run_elevated(command: &str) -> Result<u32, String> {
    let verb = wide("runas");
    let file = wide("cmd.exe");
    let params = wide(&format!("/c {command}"));
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
            return Err("The administrator prompt was cancelled or failed.".into());
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

/// Returns true if the MouseKeyLANService Windows service is installed.
pub fn service_installed() -> bool {
    std::process::Command::new("sc.exe")
        .args(["query", SERVICE_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns true if the service is currently running.
pub fn service_running() -> bool {
    let out = std::process::Command::new("sc.exe")
        .args(["query", SERVICE_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap_or_else(|_| std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: vec![],
            stderr: vec![],
        });
    let text = String::from_utf8_lossy(&out.stdout);
    text.contains("RUNNING")
}

/// Find mkl-service.exe bundled next to the main executable.
fn find_service_exe() -> Result<std::path::PathBuf, String> {
    // 1. Next to the current exe (dev builds / portable installs)
    if let Ok(mut p) = std::env::current_exe() {
        p.set_file_name("mkl-service.exe");
        if p.exists() {
            return Ok(p);
        }
    }
    // 2. Already installed to Program Files
    let installed = std::path::Path::new(INSTALL_DIR).join("mkl-service.exe");
    if installed.exists() {
        return Ok(installed);
    }
    Err(format!(
        "mkl-service.exe not found next to MouseKey LAN.exe or in {INSTALL_DIR}"
    ))
}

/// Install and start the service (triggers a UAC popup).
pub fn install() -> Result<(), String> {
    let src = find_service_exe()?;
    let src_str = src.display().to_string();

    let script = format!(
        r#"mkdir "{INSTALL_DIR}" 2>nul & copy /Y "{src_str}" "{INSTALL_DIR}\mkl-service.exe" && sc stop {SERVICE_NAME} 2>nul & sc delete {SERVICE_NAME} 2>nul & sc create {SERVICE_NAME} binPath= "{INSTALL_DIR}\mkl-service.exe" start= auto DisplayName= "MouseKey LAN Lock Service" && sc description {SERVICE_NAME} "Allows MouseKey LAN to send input on the Windows lock screen" && sc start {SERVICE_NAME}"#
    );

    let code = run_elevated(&script)?;
    if code != 0 {
        return Err(format!("Service installation failed (exit {code})."));
    }
    Ok(())
}

/// Stop and remove the service (triggers a UAC popup).
pub fn uninstall() -> Result<(), String> {
    let script = format!("sc stop {SERVICE_NAME} 2>nul & sc delete {SERVICE_NAME}");
    let code = run_elevated(&script)?;
    if code != 0 {
        return Err(format!("Service removal failed (exit {code})."));
    }
    Ok(())
}
