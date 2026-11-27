//! The SYSTEM agent in the user's session: serves the lock-screen pipe for the MouseKey LAN app
//! and injects what it receives on the secure desktop, only while the session is locked.

use std::mem::{size_of, size_of_val};
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use mkl_input::secure::{self, PIPE_NAME, RECORD, REPLY_FAILED, REPLY_INVALID, REPLY_OK, REPLY_REFUSED};
use mkl_input::InputEvent;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_PIPE_CONNECTED, GENERIC_ALL, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, SetThreadDesktop, HDESK, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, OpenProcess, WaitForSingleObject, INFINITE};

use crate::wide;

/// Desktop Windows uses for the lock, sign-in and UAC screens.
const SECURE_DESKTOP: &str = "Winlogon";
/// Only SYSTEM and elevated administrators may open the pipe.
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)";
const LOCK_RECHECK: Duration = Duration::from_millis(250);
const SYNCHRONIZE: u32 = 0x0010_0000;

pub fn run(service_pid: Option<u32>) -> i32 {
    crate::init_logging("agent");
    mkl_input::init_process_dpi();
    if let Some(pid) = service_pid {
        exit_with_service(pid);
    }
    let mut session = 0u32;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    tracing::info!(session, "agent started");
    let mut desktop = SecureDesktop { session, attached: null_mut(), locked: false, checked: None };
    loop {
        match create_pipe() {
            Ok(pipe) => serve(&pipe, &mut desktop),
            Err(err) => {
                // The name is taken: the app refuses any pipe that isn't run by SYSTEM.
                tracing::warn!(err, "could not create the lock-screen pipe; retrying");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

/// Exits when the service process ends, so no agent outlives it.
fn exit_with_service(pid: u32) {
    let service = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if service.is_null() {
        return;
    }
    let service = service as usize;
    std::thread::spawn(move || {
        unsafe { WaitForSingleObject(service as HANDLE, INFINITE) };
        tracing::info!("service ended; agent exiting");
        std::process::exit(0);
    });
}

struct Pipe(HANDLE);

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn create_pipe() -> Result<Pipe, u32> {
    unsafe {
        let sddl = wide(PIPE_SDDL);
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut()) == 0 {
            return Err(GetLastError());
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let name = wide(PIPE_NAME);
        // First instance only: never join a pipe someone else created under this name.
        let pipe = CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64,
            64,
            0,
            &sa,
        );
        let err = GetLastError();
        LocalFree(sd);
        if pipe == INVALID_HANDLE_VALUE {
            Err(err)
        } else {
            Ok(Pipe(pipe))
        }
    }
}

/// Serves one app connection at a time, forever.
fn serve(pipe: &Pipe, desktop: &mut SecureDesktop) -> ! {
    loop {
        let connected =
            unsafe { ConnectNamedPipe(pipe.0, null_mut()) } != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if connected {
            let mut client = 0u32;
            unsafe { GetNamedPipeClientProcessId(pipe.0, &mut client) };
            tracing::info!(client, "app connected");
            let (mut injected, mut refused) = (0u64, 0u64);
            let mut rec = [0u8; RECORD];
            while unsafe { secure::read_exact(pipe.0, &mut rec) } {
                let reply = match secure::decode(&rec) {
                    Some(ev) => desktop.inject(&ev),
                    None => REPLY_INVALID,
                };
                if reply == REPLY_OK {
                    injected += 1;
                } else {
                    refused += 1;
                }
                if !unsafe { secure::write_all(pipe.0, &[reply]) } {
                    break;
                }
            }
            tracing::info!(client, injected, refused, "app disconnected");
        } else {
            std::thread::sleep(Duration::from_millis(100));
        }
        unsafe { DisconnectNamedPipe(pipe.0) };
    }
}

struct SecureDesktop {
    session: u32,
    /// Desktop this thread is attached to (null until the first injection).
    attached: HDESK,
    locked: bool,
    checked: Option<Instant>,
}

impl SecureDesktop {
    /// Injects `ev` only while the session is locked and the secure desktop has the input.
    /// Everywhere else the app injects by itself, so UAC prompts in an unlocked session are
    /// never driven from here.
    fn inject(&mut self, ev: &InputEvent) -> u8 {
        if !self.session_locked() {
            return REPLY_REFUSED;
        }
        let Some((desk, name)) = input_desktop() else { return REPLY_FAILED };
        if !name.eq_ignore_ascii_case(SECURE_DESKTOP) {
            unsafe { CloseDesktop(desk) };
            return REPLY_REFUSED;
        }
        let first = self.attached.is_null();
        if first && !self.attach(desk) {
            return REPLY_FAILED;
        }
        if mkl_input::send_input(ev).is_ok() {
            if !first {
                unsafe { CloseDesktop(desk) };
            }
            return REPLY_OK;
        }
        // The secure desktop may have been recreated: attach to the current one, retry once.
        if !first && self.attach(desk) && mkl_input::send_input(ev).is_ok() {
            return REPLY_OK;
        }
        REPLY_FAILED
    }

    /// Moves this thread to `desk` (taking ownership of the handle).
    fn attach(&mut self, desk: HDESK) -> bool {
        if unsafe { SetThreadDesktop(desk) } == 0 {
            tracing::warn!(err = unsafe { GetLastError() }, "could not switch to the secure desktop");
            unsafe { CloseDesktop(desk) };
            return false;
        }
        if !self.attached.is_null() {
            unsafe { CloseDesktop(self.attached) };
        }
        self.attached = desk;
        tracing::info!("delivering input to the secure desktop");
        true
    }

    fn session_locked(&mut self) -> bool {
        if !self.checked.is_some_and(|t| t.elapsed() < LOCK_RECHECK) {
            self.locked = secure::session_locked(self.session) == Some(true);
            self.checked = Some(Instant::now());
        }
        self.locked
    }
}

/// The desktop that currently receives input, and its name.
fn input_desktop() -> Option<(HDESK, String)> {
    unsafe {
        let desk = OpenInputDesktop(0, 0, GENERIC_ALL);
        if desk.is_null() {
            return None;
        }
        let mut name = [0u16; 64];
        let mut needed = 0u32;
        if GetUserObjectInformationW(desk, UOI_NAME, name.as_mut_ptr().cast(), size_of_val(&name) as u32, &mut needed) == 0 {
            CloseDesktop(desk);
            return None;
        }
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        Some((desk, String::from_utf16_lossy(&name[..len])))
    }
}
