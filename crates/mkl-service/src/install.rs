//! Installs, repairs and removes the service (administrator rights required). The Settings
//! toggle runs `--install` / `--uninstall`; the installer runs `--upgrade` / `--uninstall`.

use std::mem::zeroed;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{GetLastError, ERROR_SERVICE_ALREADY_RUNNING};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService, CreateServiceW, DeleteService,
    OpenSCManagerW, OpenServiceW, QueryServiceStatus, StartServiceW, SC_ACTION, SC_ACTION_RESTART, SC_HANDLE,
    SC_MANAGER_ALL_ACCESS, SC_MANAGER_CONNECT, SERVICE_ALL_ACCESS, SERVICE_AUTO_START, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW,
    SERVICE_ERROR_NORMAL, SERVICE_FAILURE_ACTIONSW, SERVICE_QUERY_STATUS, SERVICE_REQUIRED_PRIVILEGES_INFOW,
    SERVICE_STATUS, SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS,
};

use crate::service::SERVICE_NAME;
use crate::wide;

const DISPLAY_NAME: &str = "MouseKey LAN Lock Service";
const DESCRIPTION: &str = "Lets MouseKey LAN deliver a paired PC's mouse and keyboard to this PC's lock screen.";
/// Scheduled task of an earlier lock-screen helper; removed whenever the service is set up.
const OLD_TASK: &str = "MouseKey LAN Lock Helper";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Exit code for the installer and the Settings toggle; errors go to stderr and the log.
pub fn cli(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("{e}");
            1
        }
    }
}

/// Installs (or repairs) the service and starts it.
pub fn install() -> Result<(), String> {
    crate::init_logging("service");
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    require_program_files(&exe)?;
    // Quoted: unquoted, a path with spaces could start C:\Program.exe instead.
    let bin = wide(&format!("\"{}\" --service", exe.display()));
    let display = wide(DISPLAY_NAME);
    unsafe {
        let scm = Handle(OpenSCManagerW(null(), null(), SC_MANAGER_ALL_ACCESS));
        if scm.0.is_null() {
            return Err(format!("Administrator rights are needed (error {}).", GetLastError()));
        }
        let svc = match Handle::open(&scm, SERVICE_ALL_ACCESS) {
            Some(svc) => {
                stop(&svc);
                let (account, no_password) = (wide("LocalSystem"), wide(""));
                let changed = ChangeServiceConfigW(
                    svc.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    bin.as_ptr(),
                    null(),
                    null_mut(),
                    null(),
                    account.as_ptr(),
                    no_password.as_ptr(),
                    display.as_ptr(),
                );
                if changed == 0 {
                    return Err(format!("Could not update the service (error {}).", GetLastError()));
                }
                svc
            }
            None => {
                let svc = Handle(CreateServiceW(
                    scm.0,
                    wide(SERVICE_NAME).as_ptr(),
                    display.as_ptr(),
                    SERVICE_ALL_ACCESS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    bin.as_ptr(),
                    null(),
                    null_mut(),
                    null(),
                    null(), // LocalSystem
                    null(),
                ));
                if svc.0.is_null() {
                    return Err(format!("Could not create the service (error {}).", GetLastError()));
                }
                svc
            }
        };
        configure(&svc);
        if StartServiceW(svc.0, 0, null()) == 0 {
            let err = GetLastError();
            if err != ERROR_SERVICE_ALREADY_RUNNING {
                return Err(format!("The service was installed but did not start (error {err})."));
            }
        }
    }
    remove_old_task();
    tracing::info!("service installed and started");
    Ok(())
}

/// Installer: repairs and restarts the service if it is installed, and does nothing else.
pub fn upgrade() -> Result<(), String> {
    let installed = unsafe {
        let scm = Handle(OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT));
        !scm.0.is_null() && Handle::open(&scm, SERVICE_QUERY_STATUS).is_some()
    };
    if installed {
        install()
    } else {
        remove_old_task();
        Ok(())
    }
}

/// Stops and deletes the service.
pub fn uninstall() -> Result<(), String> {
    crate::init_logging("service");
    unsafe {
        let scm = Handle(OpenSCManagerW(null(), null(), SC_MANAGER_ALL_ACCESS));
        if scm.0.is_null() {
            return Err(format!("Administrator rights are needed (error {}).", GetLastError()));
        }
        if let Some(svc) = Handle::open(&scm, SERVICE_ALL_ACCESS) {
            stop(&svc);
            if DeleteService(svc.0) == 0 {
                return Err(format!("Could not remove the service (error {}).", GetLastError()));
            }
        }
    }
    remove_old_task();
    tracing::info!("service removed");
    Ok(())
}

struct Handle(SC_HANDLE);

impl Handle {
    unsafe fn open(scm: &Handle, access: u32) -> Option<Handle> {
        let svc = OpenServiceW(scm.0, wide(SERVICE_NAME).as_ptr(), access);
        (!svc.is_null()).then_some(Handle(svc))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseServiceHandle(self.0);
            }
        }
    }
}

/// Stops the service and waits up to 15 s for it (and so its agent) to end.
unsafe fn stop(svc: &Handle) {
    let mut status: SERVICE_STATUS = zeroed();
    if ControlService(svc.0, SERVICE_CONTROL_STOP, &mut status) == 0 {
        return; // not running
    }
    for _ in 0..60 {
        if QueryServiceStatus(svc.0, &mut status) == 0 || status.dwCurrentState == SERVICE_STOPPED {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

unsafe fn configure(svc: &Handle) {
    let mut description = wide(DESCRIPTION);
    let info = SERVICE_DESCRIPTIONW { lpDescription: description.as_mut_ptr() };
    ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_DESCRIPTION, (&info as *const SERVICE_DESCRIPTIONW).cast());

    let mut actions = [5_000, 10_000, 60_000].map(|delay| SC_ACTION { Type: SC_ACTION_RESTART, Delay: delay });
    let failure = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 86_400,
        lpRebootMsg: null_mut(),
        lpCommand: null_mut(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_FAILURE_ACTIONS, (&failure as *const SERVICE_FAILURE_ACTIONSW).cast());

    // Only what starting the agent in the user's session needs.
    let mut privileges: Vec<u16> =
        "SeTcbPrivilege\0SeAssignPrimaryTokenPrivilege\0SeIncreaseQuotaPrivilege\0SeChangeNotifyPrivilege\0\0"
            .encode_utf16()
            .collect();
    let required = SERVICE_REQUIRED_PRIVILEGES_INFOW { pmszRequiredPrivileges: privileges.as_mut_ptr() };
    ChangeServiceConfig2W(
        svc.0,
        SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
        (&required as *const SERVICE_REQUIRED_PRIVILEGES_INFOW).cast(),
    );
}

/// A SYSTEM service must run from a folder only administrators can change.
fn require_program_files(exe: &Path) -> Result<(), String> {
    let exe = exe.to_string_lossy().to_lowercase();
    let inside = ["ProgramW6432", "ProgramFiles"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .any(|dir| exe.starts_with(&format!("{}\\", dir.trim_end_matches('\\').to_lowercase())));
    if inside {
        Ok(())
    } else {
        Err("Install MouseKey LAN with its installer first: the lock-screen service only runs from its Program Files folder.".into())
    }
}

fn remove_old_task() {
    let _ = std::process::Command::new("schtasks.exe")
        .args(["/Delete", "/F", "/TN", OLD_TASK])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}
