//! Input on the Windows secure desktop (the password screen of a locked PC).
//!
//! Windows only accepts input there from SYSTEM processes attached to that desktop, so
//! `SendInput` from the per-user app fails with `ERROR_ACCESS_DENIED`. The optional lock-screen
//! service (`mkl-service`) keeps a small SYSTEM agent running in the user's session. This module
//! holds the wire format both sides use and the app's side of the connection.
//!
//! * The agent's pipe admits only SYSTEM and elevated administrators, and the app only talks to
//!   a pipe served by SYSTEM, so nothing else can pose as the agent and read keystrokes.
//! * The agent injects only while the session is locked and the secure desktop has the input.

use std::mem::{size_of, size_of_val};
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{GetTokenInformation, IsWellKnownSid, TokenUser, WinLocalSystemSid, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::RemoteDesktop::{
    WTSFreeMemory, WTSQuerySessionInformationW, WTSSessionInfoEx, WTSINFOEXW, WTS_CURRENT_SERVER_HANDLE,
    WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNLOCK,
};
use windows_sys::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

use crate::{InputEvent, MouseButton};

/// Named pipe served by the agent.
pub const PIPE_NAME: &str = r"\\.\pipe\MouseKeyLAN.SecureInput";
/// Size of one encoded event; the agent answers each with one reply byte.
pub const RECORD: usize = 12;
pub const REPLY_OK: u8 = 0;
/// Not injected: the session isn't locked or the secure desktop doesn't have the input.
pub const REPLY_REFUSED: u8 = 1;
pub const REPLY_FAILED: u8 = 2;
pub const REPLY_INVALID: u8 = 3;

const MAX_WHEEL: i32 = 120 * 40;
const RETRY: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);

/// Layout: tag, two flags, a zero byte, then two little-endian `i32`s.
pub fn encode(ev: &InputEvent) -> [u8; RECORD] {
    let (tag, f1, f2, a, b) = match *ev {
        InputEvent::MouseMove { x, y } => (1, 0, 0, x, y),
        InputEvent::Button { button, down } => (2, down as u8, button_code(button), 0, 0),
        InputEvent::Wheel { delta, horizontal } => (3, horizontal as u8, 0, delta, 0),
        InputEvent::Key { vk, scan, extended, down } => (4, extended as u8, down as u8, vk as i32, scan as i32),
    };
    let mut r = [0u8; RECORD];
    r[..3].copy_from_slice(&[tag, f1, f2]);
    r[4..8].copy_from_slice(&a.to_le_bytes());
    r[8..].copy_from_slice(&b.to_le_bytes());
    r
}

/// Strict inverse of [`encode`]: anything malformed or out of range is rejected.
pub fn decode(r: &[u8; RECORD]) -> Option<InputEvent> {
    let flag = |v: u8| match v {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    };
    let a = i32::from_le_bytes([r[4], r[5], r[6], r[7]]);
    let b = i32::from_le_bytes([r[8], r[9], r[10], r[11]]);
    if r[3] != 0 {
        return None;
    }
    match r[0] {
        1 if r[1] == 0 && r[2] == 0 => Some(InputEvent::MouseMove { x: a, y: b }),
        2 if a == 0 && b == 0 => Some(InputEvent::Button { button: button_from(r[2])?, down: flag(r[1])? }),
        3 if r[2] == 0 && b == 0 && (-MAX_WHEEL..=MAX_WHEEL).contains(&a) => {
            Some(InputEvent::Wheel { delta: a, horizontal: flag(r[1])? })
        }
        4 => {
            let (vk, scan) = (u16::try_from(a).ok()?, u16::try_from(b).ok()?);
            if vk > 0xFF || scan > 0x1FF || (vk == 0 && scan == 0) {
                return None;
            }
            Some(InputEvent::Key { vk, scan, extended: flag(r[1])?, down: flag(r[2])? })
        }
        _ => None,
    }
}

fn button_code(b: MouseButton) -> u8 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::X1 => 3,
        MouseButton::X2 => 4,
    }
}

fn button_from(code: u8) -> Option<MouseButton> {
    Some(match code {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        3 => MouseButton::X1,
        4 => MouseButton::X2,
        _ => return None,
    })
}

/// Lock state of a Windows session (`None` if Windows can't tell).
pub fn session_locked(session: u32) -> Option<bool> {
    let mut buf = null_mut();
    let mut len = 0u32;
    unsafe {
        if WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, session, WTSSessionInfoEx, &mut buf, &mut len) == 0
            || buf.is_null()
        {
            return None;
        }
        let info = &*(buf as *const WTSINFOEXW);
        let state = if info.Level == 1 && len as usize >= size_of::<WTSINFOEXW>() {
            match info.Data.WTSInfoExLevel1.SessionFlags as u32 {
                WTS_SESSIONSTATE_LOCK => Some(true),
                WTS_SESSIONSTATE_UNLOCK => Some(false),
                _ => None,
            }
        } else {
            None
        };
        WTSFreeMemory(buf.cast());
        state
    }
}

/// Writes all of `buf` to a pipe.
///
/// # Safety
/// `pipe` must be an open pipe handle.
pub unsafe fn write_all(pipe: HANDLE, mut buf: &[u8]) -> bool {
    while !buf.is_empty() {
        let mut n = 0u32;
        if WriteFile(pipe, buf.as_ptr(), buf.len() as u32, &mut n, null_mut()) == 0 || n == 0 {
            return false;
        }
        buf = &buf[n as usize..];
    }
    true
}

/// Fills `buf` from a pipe.
///
/// # Safety
/// `pipe` must be an open pipe handle.
pub unsafe fn read_exact(pipe: HANDLE, buf: &mut [u8]) -> bool {
    let mut done = 0;
    while done < buf.len() {
        let mut n = 0u32;
        let rest = &mut buf[done..];
        if ReadFile(pipe, rest.as_mut_ptr(), rest.len() as u32, &mut n, null_mut()) == 0 || n == 0 {
            return false;
        }
        done += n as usize;
    }
    true
}

// ---------------------------------------------------------------------------------------
// App side
// ---------------------------------------------------------------------------------------

/// Why the agent can't be used right now (logged once per change).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unavailable {
    /// The service isn't installed or not running.
    NotRunning,
    /// This app isn't running as administrator.
    NeedsAdmin,
    /// Another program holds the agent's only connection.
    Busy,
    /// The pipe isn't served by SYSTEM: refused.
    NotSystem,
    Os(u32),
}

struct Link {
    requests: SyncSender<[u8; RECORD]>,
    replies: Receiver<u8>,
}

struct State {
    link: Option<Link>,
    retry_at: Option<Instant>,
    problem: Option<Unavailable>,
}

static STATE: Mutex<State> = Mutex::new(State { link: None, retry_at: None, problem: None });

struct Pipe(HANDLE);

// The handle is only used by the connection's own thread.
unsafe impl Send for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The lock-screen agent is reachable (connects if needed, at most every 2 s).
pub fn ready() -> bool {
    connected(&mut state())
}

/// Injects `ev` through the agent. False if it isn't reachable or didn't inject.
pub(crate) fn forward(ev: &InputEvent) -> bool {
    let mut st = state();
    // A second attempt covers an agent that restarted since the last event.
    for _ in 0..2 {
        if !connected(&mut st) {
            return false;
        }
        let link = st.link.as_ref().expect("connected");
        let reply = link.requests.send(encode(ev)).ok().and_then(|_| link.replies.recv_timeout(REPLY_TIMEOUT).ok());
        match reply {
            Some(code) => return code == REPLY_OK,
            None => {
                tracing::info!("lock-screen service connection lost");
                st.link = None;
            }
        }
    }
    false
}

fn connected(st: &mut State) -> bool {
    if st.link.is_some() {
        return true;
    }
    if st.retry_at.is_some_and(|t| Instant::now() < t) {
        return false;
    }
    match connect() {
        Ok(link) => {
            tracing::info!("connected to the lock-screen service");
            st.link = Some(link);
            st.retry_at = None;
            st.problem = None;
            true
        }
        Err(problem) => {
            if st.problem != Some(problem) {
                match problem {
                    Unavailable::NotSystem => tracing::warn!("refused a lock-screen service pipe not run by SYSTEM"),
                    Unavailable::NotRunning => tracing::debug!("lock-screen service not running"),
                    other => tracing::info!(?other, "lock-screen service unavailable"),
                }
                st.problem = Some(problem);
            }
            st.retry_at = Some(Instant::now() + RETRY);
            false
        }
    }
}

fn connect() -> Result<Link, Unavailable> {
    let name: Vec<u16> = PIPE_NAME.encode_utf16().chain(Some(0)).collect();
    // Identification only: the server may check who we are but can't act as us.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(match unsafe { GetLastError() } {
            ERROR_FILE_NOT_FOUND => Unavailable::NotRunning,
            ERROR_ACCESS_DENIED => Unavailable::NeedsAdmin,
            ERROR_PIPE_BUSY => Unavailable::Busy,
            e => Unavailable::Os(e),
        });
    }
    let pipe = Pipe(h);
    let mut pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(h, &mut pid) } == 0 || !runs_as_system(pid) {
        return Err(Unavailable::NotSystem);
    }
    let (requests, incoming) = mpsc::sync_channel::<[u8; RECORD]>(1);
    let (reply_tx, replies) = mpsc::sync_channel::<u8>(1);
    std::thread::Builder::new()
        .name("mkl-lock-screen".into())
        .spawn(move || {
            let pipe = pipe;
            for rec in incoming {
                let mut reply = [0u8; 1];
                let exchanged = unsafe { write_all(pipe.0, &rec) && read_exact(pipe.0, &mut reply) };
                if !exchanged || reply_tx.send(reply[0]).is_err() {
                    break;
                }
            }
        })
        .map_err(|_| Unavailable::Os(0))?;
    Ok(Link { requests, replies })
}

/// The process runs as LocalSystem.
fn runs_as_system(pid: u32) -> bool {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut token = null_mut();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token) != 0;
        CloseHandle(process);
        if !opened {
            return false;
        }
        // TOKEN_USER plus the SID it points to; u64 keeps it aligned.
        let mut buf = [0u64; 16];
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), size_of_val(&buf) as u32, &mut len) != 0;
        CloseHandle(token);
        ok && IsWellKnownSid((*(buf.as_ptr() as *const TOKEN_USER)).User.Sid, WinLocalSystemSid) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_round_trips() {
        let events = [
            InputEvent::MouseMove { x: -3840, y: 2159 },
            InputEvent::MouseMove { x: i32::MIN, y: i32::MAX },
            InputEvent::Button { button: MouseButton::X2, down: true },
            InputEvent::Button { button: MouseButton::Left, down: false },
            InputEvent::Wheel { delta: -240, horizontal: true },
            InputEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down: true },
            InputEvent::Key { vk: 0, scan: 0x1D, extended: true, down: false },
        ];
        for ev in events {
            assert_eq!(decode(&encode(&ev)), Some(ev), "{ev:?}");
        }
    }

    #[test]
    fn malformed_records_are_rejected() {
        let key = encode(&InputEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down: true });
        let bad = |f: &dyn Fn(&mut [u8; RECORD])| {
            let mut r = key;
            f(&mut r);
            decode(&r)
        };
        assert_eq!(bad(&|r| r[0] = 9), None, "unknown tag");
        assert_eq!(bad(&|r| r[3] = 1), None, "reserved byte");
        assert_eq!(bad(&|r| r[2] = 2), None, "flag out of range");
        assert_eq!(bad(&|r| r[4..8].copy_from_slice(&0x100i32.to_le_bytes())), None, "vk out of range");
        assert_eq!(bad(&|r| r[8..].copy_from_slice(&(-1i32).to_le_bytes())), None, "negative scan");
        assert_eq!(decode(&encode(&InputEvent::Key { vk: 0, scan: 0, extended: false, down: true })), None);
        assert_eq!(decode(&encode(&InputEvent::Wheel { delta: MAX_WHEEL + 1, horizontal: false })), None);
        let mut button = encode(&InputEvent::Button { button: MouseButton::Left, down: true });
        button[2] = 5;
        assert_eq!(decode(&button), None, "unknown button");
    }

    #[test]
    fn this_session_reports_a_lock_state() {
        // Tests run in an unlocked interactive session.
        let current = windows_sys::Win32::System::RemoteDesktop::WTS_CURRENT_SESSION;
        assert_eq!(session_locked(current), Some(false));
    }
}
