//! MouseKey LAN lock-screen service.
//!
//! Windows shows the password screen of a locked PC on its secure desktop, where only SYSTEM
//! processes may inject input. This one executable has three roles:
//!
//! * `--service`: the Windows service (LocalSystem). It keeps one agent running as SYSTEM in the
//!   active console session, restarting it if it exits and moving it if the session changes.
//! * `--agent <service pid>`: serves the lock-screen pipe for the MouseKey LAN app and injects
//!   what it receives on the secure desktop, only while the session is locked
//!   (see `mkl_input::secure`).
//! * `--install`, `--upgrade`, `--uninstall`: used by the Settings toggle and the installer
//!   (administrator rights required).
//!
//! Nothing here touches the network, files the user owns, the clipboard or credentials.

#[cfg(windows)]
mod agent;
#[cfg(windows)]
mod install;
#[cfg(windows)]
mod service;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None | Some("--service") => service::run(),
        Some("--agent") => agent::run(args.get(1).and_then(|pid| pid.parse().ok())),
        Some("--install") => install::cli(install::install()),
        Some("--upgrade") => install::cli(install::upgrade()),
        Some("--uninstall") => install::cli(install::uninstall()),
        Some(_) => {
            eprintln!("usage: mkl-service [--install | --upgrade | --uninstall]");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("mkl-service is only needed on Windows");
    std::process::exit(2);
}

/// Logs to `logs\<name>.log` next to the executable: in Program Files only administrators and
/// SYSTEM can write there, so nobody can redirect what SYSTEM writes. Input is never logged.
#[cfg(windows)]
fn init_logging(name: &str) {
    use std::fs::{self, OpenOptions};
    use std::sync::Mutex;

    const MAX_LOG_BYTES: u64 = 1024 * 1024;
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("logs"))) else {
        return;
    };
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.log"));
    if fs::metadata(&path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let _ = fs::rename(&path, dir.join(format!("{name}.old.log")));
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = tracing_subscriber::fmt().with_ansi(false).with_writer(Mutex::new(file)).try_init();
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
