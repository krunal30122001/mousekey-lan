//! Lock-screen service management (Settings toggle).
//!
//! The service is `mkl-service.exe` next to the app (see crates/mkl-service): a LocalSystem
//! Windows service that keeps a SYSTEM agent in the user's session, so input can reach the
//! password screen of a locked PC. Installing and removing it needs administrator rights, so
//! both run `mkl-service.exe` behind a UAC prompt.

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

const SERVICE_NAME: &str = "MouseKeyLANService";
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

/// Runs `file params` elevated (UAC prompt) and returns its exit code.
fn run_elevated(file: &Path, params: &str) -> Result<u32, String> {
    let verb = wide("runas");
    let file = wide(&file.to_string_lossy());
    let params = wide(params);
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

/// `mkl-service.exe`, installed next to the app.
fn service_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = exe.with_file_name("mkl-service.exe");
    if path.exists() {
        Ok(path)
    } else {
        Err("mkl-service.exe is missing. Reinstall MouseKey LAN with its installer.".into())
    }
}

fn run_service_exe(arg: &str, action: &str) -> Result<(), String> {
    let exe = service_exe()?;
    match run_elevated(&exe, arg)? {
        0 => Ok(()),
        code => {
            let log = exe.with_file_name("logs").join("service.log");
            Err(format!("Could not {action} the lock-screen service (exit {code}). Details: {}", log.display()))
        }
    }
}

/// Installs and starts the service.
pub fn install() -> Result<(), String> {
    run_service_exe("--install", "install")
}

/// Stops and removes the service.
pub fn uninstall() -> Result<(), String> {
    run_service_exe("--uninstall", "remove")
}

/// (installed, running), from the Service Control Manager.
pub fn service_state() -> (bool, bool) {
    let Ok(out) = std::process::Command::new("sc.exe")
        .args(["query", SERVICE_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return (false, false);
    };
    let installed = out.status.success();
    (installed, installed && String::from_utf8_lossy(&out.stdout).contains("RUNNING"))
}
