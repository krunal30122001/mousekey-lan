//! The Windows service (LocalSystem): keeps one agent running as SYSTEM in the active console
//! session, restarting it if it exits and moving it when the console session changes.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::{Duration, Instant};

use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_CALL_NOT_IMPLEMENTED, ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, HANDLE, NO_ERROR,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, DuplicateTokenEx, LookupPrivilegeValueW, SecurityImpersonation, SetTokenInformation,
    TokenPrimary, TokenSessionId, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_DUPLICATE,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW, SERVICE_ACCEPT_SHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW,
    SERVICE_WIN32_OWN_PROCESS,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessAsUserW, GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess, OpenProcessToken,
    SetEvent, TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::wide;

pub const SERVICE_NAME: &str = "MouseKeyLANService";

/// `WTSGetActiveConsoleSessionId` while no session is attached to the console.
const NO_SESSION: u32 = 0xFFFF_FFFF;
const MAX_DELAY: Duration = Duration::from_secs(30);
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

static STATUS: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static STOP: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

/// Hands the process to the Service Control Manager; returns when the service has stopped.
pub fn run() -> i32 {
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: name.as_mut_ptr(), lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW { lpServiceName: null_mut(), lpServiceProc: None },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } != 0 {
        return 0;
    }
    if unsafe { GetLastError() } == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT {
        eprintln!("mkl-service runs as a Windows service; use --install to install it");
        return 2;
    }
    1
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    crate::init_logging("service");
    let name = wide(SERVICE_NAME);
    let status = RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control), null());
    if status.is_null() {
        tracing::error!(err = GetLastError(), "could not register the service control handler");
        return;
    }
    STATUS.store(status, Ordering::Release);
    let stop = CreateEventW(null(), 1, 0, null());
    if stop.is_null() {
        tracing::error!(err = GetLastError(), "could not create the stop event");
        report(SERVICE_STOPPED);
        return;
    }
    STOP.store(stop, Ordering::Release);
    report(SERVICE_RUNNING);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "service started");
    enable_privileges();
    supervise(stop);
    tracing::info!("service stopped");
    report(SERVICE_STOPPED);
}

unsafe extern "system" fn control(code: u32, _event: u32, _data: *mut c_void, _context: *mut c_void) -> u32 {
    match code {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            report(SERVICE_STOP_PENDING);
            SetEvent(STOP.load(Ordering::Acquire));
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

fn report(state: SERVICE_STATUS_CURRENT_STATE) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN } else { 0 },
        dwWin32ExitCode: NO_ERROR,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: if state == SERVICE_STOP_PENDING { 5_000 } else { 0 },
    };
    unsafe {
        SetServiceStatus(STATUS.load(Ordering::Acquire), &status);
    }
}

struct Agent {
    process: HANDLE,
    session: u32,
    started: Instant,
}

/// Runs until `stop` is signalled, then ends the agent.
fn supervise(stop: HANDLE) {
    let mut agent: Option<Agent> = None;
    let mut delay = Duration::from_secs(1);
    let mut next_start = Instant::now();
    loop {
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        if let Some(a) = agent.take() {
            if unsafe { WaitForSingleObject(a.process, 0) } == WAIT_OBJECT_0 {
                let mut code = 0u32;
                unsafe { GetExitCodeProcess(a.process, &mut code) };
                tracing::warn!(code, session = a.session, "agent exited");
                // Back off when it keeps exiting right after starting.
                delay = if a.started.elapsed() < Duration::from_secs(10) {
                    (delay * 2).min(MAX_DELAY)
                } else {
                    Duration::from_secs(1)
                };
                next_start = Instant::now() + delay;
                unsafe { CloseHandle(a.process) };
            } else if a.session != console {
                tracing::info!(from = a.session, to = console, "console session changed");
                unsafe {
                    TerminateProcess(a.process, 0);
                    CloseHandle(a.process);
                }
            } else {
                agent = Some(a);
            }
        }
        if agent.is_none() && console != NO_SESSION && Instant::now() >= next_start {
            match unsafe { spawn_agent(console) } {
                Ok(process) => {
                    tracing::info!(session = console, "agent started");
                    agent = Some(Agent { process, session: console, started: Instant::now() });
                }
                Err(err) => {
                    tracing::warn!(err, session = console, "could not start the agent");
                    delay = (delay * 2).min(MAX_DELAY);
                    next_start = Instant::now() + delay;
                }
            }
        }
        if unsafe { WaitForSingleObject(stop, 1000) } == WAIT_OBJECT_0 {
            break;
        }
    }
    if let Some(a) = agent {
        unsafe {
            TerminateProcess(a.process, 0);
            CloseHandle(a.process);
        }
    }
}

/// Starts `--agent` as SYSTEM in `session`, with a copy of this service's token moved there.
unsafe fn spawn_agent(session: u32) -> Result<HANDLE, u32> {
    let exe = std::env::current_exe().map_err(|e| e.raw_os_error().unwrap_or(0) as u32)?;
    let mut own = null_mut();
    if OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut own) == 0 {
        return Err(GetLastError());
    }
    let mut token = null_mut();
    let duplicated = DuplicateTokenEx(own, MAXIMUM_ALLOWED, null(), SecurityImpersonation, TokenPrimary, &mut token) != 0;
    CloseHandle(own);
    if !duplicated {
        return Err(GetLastError());
    }
    let started = start_in_session(token, session, &exe);
    CloseHandle(token);
    started
}

unsafe fn start_in_session(token: HANDLE, session: u32, exe: &Path) -> Result<HANDLE, u32> {
    if SetTokenInformation(token, TokenSessionId, (&session as *const u32).cast(), size_of::<u32>() as u32) == 0 {
        return Err(GetLastError());
    }
    let app = wide(&exe.to_string_lossy());
    let mut cmd = wide(&format!("\"{}\" --agent {}", exe.display(), GetCurrentProcessId()));
    let mut desktop = wide("winsta0\\default");
    let mut si: STARTUPINFOW = zeroed();
    si.cb = size_of::<STARTUPINFOW>() as u32;
    si.lpDesktop = desktop.as_mut_ptr();
    let mut pi: PROCESS_INFORMATION = zeroed();
    let created = CreateProcessAsUserW(
        token,
        app.as_ptr(),
        cmd.as_mut_ptr(),
        null(),
        null(),
        0,
        CREATE_NO_WINDOW,
        null(),
        null(),
        &si,
        &mut pi,
    );
    if created == 0 {
        return Err(GetLastError());
    }
    CloseHandle(pi.hThread);
    Ok(pi.hProcess)
}

/// Starting a process as SYSTEM in another session needs these; the service is restricted to
/// them when it is installed.
fn enable_privileges() {
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) == 0 {
            return;
        }
        for name in ["SeTcbPrivilege", "SeAssignPrimaryTokenPrivilege", "SeIncreaseQuotaPrivilege"] {
            let mut tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES { Luid: zeroed(), Attributes: SE_PRIVILEGE_ENABLED }],
            };
            if LookupPrivilegeValueW(null(), wide(name).as_ptr(), &mut tp.Privileges[0].Luid) != 0 {
                AdjustTokenPrivileges(token, 0, &tp, 0, null_mut(), null_mut());
            }
        }
        CloseHandle(token);
    }
}
