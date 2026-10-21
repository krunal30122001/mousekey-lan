//! Tauri commands: thin wrappers that translate between the frontend and the engine.
//! Errors are returned as user-facing strings.

use mkl_core::{Diagnostics, LayoutPos, LayoutView, Settings, Snapshot};
use qrcode::render::svg;
use qrcode::QrCode;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::sysmetrics::{self, ProcessMetrics};
use crate::AppState;

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> Snapshot {
    state.engine.snapshot()
}

#[tauri::command]
pub fn pair_with(state: State<'_, AppState>, target: String) -> CmdResult<()> {
    state.engine.pair_with(&target).map_err(err)
}

#[tauri::command]
pub fn pair_device(state: State<'_, AppState>, device_id: String) -> CmdResult<()> {
    state.engine.pair_device(&device_id).map_err(err)
}

#[tauri::command]
pub fn respond_pairing(state: State<'_, AppState>, request_id: u64, approve: bool, block: bool) -> CmdResult<()> {
    state.engine.respond_pairing(request_id, approve, block).map_err(err)
}

#[tauri::command]
pub fn disconnect_device(state: State<'_, AppState>, device_id: String) {
    state.engine.disconnect(&device_id);
}

#[tauri::command]
pub fn connect_device(state: State<'_, AppState>, device_id: String) {
    state.engine.connect(&device_id);
}

#[tauri::command]
pub fn forget_device(state: State<'_, AppState>, device_id: String) -> CmdResult<()> {
    state.engine.forget(&device_id).map_err(err)
}

#[tauri::command]
pub fn set_blocked(state: State<'_, AppState>, device_id: String, blocked: bool) -> CmdResult<()> {
    state.engine.set_blocked(&device_id, blocked).map_err(err)
}

#[tauri::command]
pub fn return_control(state: State<'_, AppState>) {
    state.engine.return_control();
}

#[tauri::command]
pub fn get_layout(state: State<'_, AppState>) -> LayoutView {
    state.engine.layout_view()
}

#[tauri::command]
pub fn save_layout(state: State<'_, AppState>, positions: Vec<LayoutPos>) -> CmdResult<()> {
    state.engine.save_layout(positions).map_err(err)
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.engine.settings()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, state: State<'_, AppState>, settings: Settings) -> CmdResult<Settings> {
    let saved = state.engine.update_settings(settings).map_err(err)?;
    let autolaunch = app.autolaunch();
    let want = saved.autostart && !crate::elevation::task_exists();
    let result = if want { autolaunch.enable() } else { autolaunch.disable() };
    result.map_err(|e| format!("Settings saved, but autostart could not be changed: {e}"))?;
    Ok(saved)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsView {
    #[serde(flatten)]
    engine: Diagnostics,
    process: ProcessMetrics,
    log_path: String,
    os: String,
}

#[tauri::command]
pub fn get_diagnostics(state: State<'_, AppState>) -> DiagnosticsView {
    DiagnosticsView {
        engine: state.engine.diagnostics(),
        process: sysmetrics::sample(),
        log_path: state.log_path.display().to_string(),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingInfo {
    uri: String,
    qr_svg: String,
    addresses: Vec<String>,
    port: u16,
    fingerprint: String,
    name: String,
}

#[tauri::command]
pub fn get_pairing_info(state: State<'_, AppState>) -> CmdResult<PairingInfo> {
    let me = state.engine.snapshot().me;
    let qr_svg = QrCode::new(me.pair_uri.as_bytes())
        .map_err(err)?
        .render::<svg::Color<'_>>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .dark_color(svg::Color("#0f172a"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(PairingInfo {
        uri: me.pair_uri,
        qr_svg,
        addresses: me.addresses,
        port: me.port,
        fingerprint: me.fingerprint,
        name: me.name,
    })
}

#[tauri::command]
pub fn open_receive_folder(state: State<'_, AppState>) -> CmdResult<String> {
    let dir = state.engine.receive_dir().to_path_buf();
    std::fs::create_dir_all(&dir).map_err(err)?;
    std::process::Command::new("explorer.exe").arg(&dir).spawn().map_err(err)?;
    Ok(dir.display().to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminMode {
    /// This running instance has administrator rights.
    elevated: bool,
    /// MouseKey LAN is set to start with administrator rights.
    enabled: bool,
}

#[tauri::command]
pub async fn get_admin_mode() -> AdminMode {
    let enabled = tauri::async_runtime::spawn_blocking(crate::elevation::task_exists).await.unwrap_or(false);
    AdminMode { elevated: mkl_input::platform_desktop().is_elevated(), enabled }
}

/// Turning it on restarts MouseKey LAN with administrator rights (after a UAC prompt).
#[tauri::command]
pub async fn set_admin_mode(app: AppHandle, enabled: bool) -> CmdResult<()> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        if enabled {
            crate::elevation::enable()
        } else {
            crate::elevation::disable()
        }
    })
    .await
    .map_err(err)?;
    result?;
    let state = app.state::<AppState>();
    if enabled {
        // The elevated copy starts in ~3 s; exit so the single-instance guard lets it run.
        let _ = app.autolaunch().disable();
        state.engine.shutdown();
        let handle = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            handle.exit(0);
        });
    } else if state.engine.settings().autostart {
        let _ = app.autolaunch().enable();
    }
    Ok(())
}

// ── Lock-screen service ────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockServiceStatus {
    /// Service is installed in the Windows Service Control Manager.
    installed: bool,
    /// Service is currently running.
    running: bool,
}

/// Return the current install/run state of the lock-screen helper service.
#[tauri::command]
pub async fn get_lock_service() -> LockServiceStatus {
    let (installed, running) = tauri::async_runtime::spawn_blocking(|| {
        (
            crate::lock_service::service_installed(),
            crate::lock_service::service_running(),
        )
    })
    .await
    .unwrap_or((false, false));
    LockServiceStatus { installed, running }
}

/// Install (enable=true) or uninstall (enable=false) the lock-screen helper service.
/// A UAC prompt is shown automatically — no manual PowerShell needed.
#[tauri::command]
pub async fn set_lock_service(enabled: bool) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        if enabled {
            crate::lock_service::install()
        } else {
            crate::lock_service::uninstall()
        }
    })
    .await
    .map_err(err)?
}
