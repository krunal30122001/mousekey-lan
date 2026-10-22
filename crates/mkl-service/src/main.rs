// mkl-service: UIAccess helper process for lock-screen input injection.
//
// This binary is NOT a Windows Service. It is started as a scheduled task
// (ONLOGON, /IT interactive) so it runs inside the user's Session 1 — the
// same session as the Winlogon secure desktop. With uiAccess="true" in its
// manifest and installed in a trusted path (Program Files), it can call
// SendInput that reaches the lock screen / UAC desktop.
//
// The main MouseKey LAN process communicates with this helper via a local
// named pipe using JSON messages.

use std::mem::size_of;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::sync::Mutex;
use serde::{Deserialize, Serialize};
use anyhow::{bail, Result};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, MOUSEINPUT,
};

const PIPE_NAME: &str = r"\\.\pipe\MouseKeyLANService";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Cmd {
    InjectKeyboard { events: Vec<KeyEv> },
    InjectMouse { dx: i32, dy: i32, flags: u32 },
    Ping,
}

#[derive(Debug, Serialize, Deserialize)]
struct KeyEv {
    vk: u16,
    scan: u16,
    flags: u32, // KEYEVENTF_* bits
}

#[derive(Debug, Serialize, Deserialize)]
struct Resp {
    ok: bool,
    msg: String,
}

#[tokio::main]
async fn main() {
    env_logger::init();
    log::info!("mkl-service started (UIAccess helper)");

    // Single named-pipe instance; loop to handle one client at a time.
    loop {
        match run_once().await {
            Ok(_) => {}
            Err(e) => log::error!("pipe error: {e}"),
        }
    }
}

async fn run_once() -> Result<()> {
    let server = Arc::new(Mutex::new(
        ServerOptions::new()
            .first_pipe_instance(false)
            .create(PIPE_NAME)?,
    ));

    // Wait for client
    server.lock().await.connect().await?;

    // Read one request (up to 64 KB)
    let mut buf = vec![0u8; 65536];
    let n = server.lock().await.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let payload = std::str::from_utf8(&buf[..n])?;
    let cmd: Cmd = serde_json::from_str(payload)?;
    let resp = handle(cmd).await;

    let json = serde_json::to_string(&resp)?;
    let mut guard = server.lock().await;
    guard.write_all(json.as_bytes()).await?;
    guard.flush().await?;
    guard.disconnect()?;
    Ok(())
}

async fn handle(cmd: Cmd) -> Resp {
    match cmd {
        Cmd::Ping => Resp { ok: true, msg: "pong".into() },
        Cmd::InjectKeyboard { events } => match inject_keys(events) {
            Ok(_) => Resp { ok: true, msg: "ok".into() },
            Err(e) => Resp { ok: false, msg: e.to_string() },
        },
        Cmd::InjectMouse { dx, dy, flags } => match inject_mouse(dx, dy, flags) {
            Ok(_) => Resp { ok: true, msg: "ok".into() },
            Err(e) => Resp { ok: false, msg: e.to_string() },
        },
    }
}

fn inject_keys(events: Vec<KeyEv>) -> Result<()> {
    let inputs: Vec<INPUT> = events
        .into_iter()
        .map(|ev| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: ev.vk,
                    wScan: ev.scan,
                    dwFlags: ev.flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
        .collect();

    let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        bail!("SendInput: sent {sent}/{}", inputs.len());
    }
    Ok(())
}

fn inject_mouse(dx: i32, dy: i32, flags: u32) -> Result<()> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    let sent = unsafe { SendInput(1, &input as *const INPUT, size_of::<INPUT>() as i32) };
    if sent != 1 {
        bail!("SendInput mouse failed");
    }
    Ok(())
}
