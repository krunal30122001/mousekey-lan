//! Optional "run as administrator" mode.
//!
//! Windows blocks input from a normal-integrity process into windows of elevated processes
//! (UIPI). To control admin windows (Task Manager, installers, admin terminals), MouseKey LAN
//! must itself run elevated. A Run-key autostart can't elevate, so this mode registers a
//! scheduled task (`/SC ONLOGON /RL HIGHEST`) for the current user, created once through the
//! standard UAC prompt. While the task exists it replaces the normal autostart entry.

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::ptr::{null, null_mut};

const TASK_NAME: &str = "MouseKey LAN (administrator)";
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

/// The elevated logon task is registered.
pub fn task_exists() -> bool {
    std::process::Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Runs `cmd.exe /c <command>` elevated (UAC prompt) and waits for it. Returns Ok(true) if it
/// succeeded, Ok(false) if it ran but failed, Err if the user declined the prompt.
fn run_elevated(command: &str) -> Result<bool, String> {
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
            return Err("The administrator prompt was cancelled.".into());
        }
        if info.process.is_null() {
            return Ok(true);
        }
        WaitForSingleObject(info.process, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(info.process, &mut code);
        CloseHandle(info.process);
        Ok(code == 0)
    }
}

/// Registers the elevated logon task and schedules an elevated restart a few seconds later
/// (the caller must exit so the elevated instance isn't stopped by the single-instance guard).
pub fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );
    let create = format!(
        "schtasks /Create /F /TN \"{TASK_NAME}\" /SC ONLOGON /RL HIGHEST /IT /RU \"{user}\" /TR \"\\\"{}\\\" --hidden\"",
        exe.display()
    );
    let restart = format!("timeout /t 3 /nobreak >nul & schtasks /Run /TN \"{TASK_NAME}\"");
    if !run_elevated(&format!("{create} && ({restart})"))? {
        return Err("Windows could not create the startup task.".into());
    }
    Ok(())
}

/// Removes the elevated logon task.
pub fn disable() -> Result<(), String> {
    if !run_elevated(&format!("schtasks /Delete /F /TN \"{TASK_NAME}\""))? {
        return Err("Windows could not remove the startup task.".into());
    }
    Ok(())
}
