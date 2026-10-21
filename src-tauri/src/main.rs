// Hide the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod elevation;
mod lock_service;
mod logging;
mod sysmetrics;
mod tray;

use std::path::PathBuf;
use std::sync::Arc;

use mkl_core::{Engine, EngineConfig, EngineEvent};
use mkl_store::Store;
use tauri::{Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

pub const HIDDEN_ARG: &str = "--hidden";

pub struct AppState {
    pub engine: Arc<Engine>,
    pub log_path: PathBuf,
}

pub fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn main() {
    // Physical-pixel coordinates everywhere, before any window exists.
    mkl_input::init_process_dpi();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main_window(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![HIDDEN_ARG])))
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let log_path = logging::init(&data_dir);
            tracing::info!(version = env!("CARGO_PKG_VERSION"), data = %data_dir.display(), "MouseKey LAN starting");

            let store = Arc::new(Store::open(data_dir.join("mousekey.db"))?);
            let (engine, mut events) = tauri::async_runtime::block_on(Engine::start(EngineConfig {
                store,
                injector: mkl_input::platform_injector(),
                listen_port: None,
                enable_capture: true,
                enable_discovery: true,
                desktop: mkl_input::platform_desktop(),
                receive_dir: app.path().download_dir()?.join("MouseKey LAN"),
                temp_dir: app.path().temp_dir()?.join("MouseKey LAN"),
            }))?;
            let engine = Arc::new(engine);

            // Keep the OS autostart entry in sync with the saved preference.
            let autolaunch = app.autolaunch();
            // The elevated scheduled task (Run as administrator) replaces the Run-key entry.
            let want = engine.settings().autostart && !elevation::task_exists();
            if autolaunch.is_enabled().unwrap_or(false) != want {
                let r = if want { autolaunch.enable() } else { autolaunch.disable() };
                if let Err(e) = r {
                    tracing::warn!("could not update autostart: {e}");
                }
            }

            app.manage(AppState { engine: engine.clone(), log_path });
            tray::create(app.handle(), engine.clone())?;

            // Forward engine events to the UI; surface the window for pairing requests.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut blocked_shown = false;
                while let Some(ev) = events.recv().await {
                    match &ev {
                        EngineEvent::PairRequest { .. } => show_main_window(&handle),
                        // First time Windows blocks typing into an admin window on the other PC:
                        // surface the explanation once instead of failing silently.
                        EngineEvent::Notice { level, .. } if level == "blocked" && !blocked_shown => {
                            blocked_shown = true;
                            show_main_window(&handle);
                        }
                        _ => {}
                    }
                    let _ = handle.emit("engine-event", &ev);
                }
            });

            let hidden = std::env::args().any(|a| a == HIDDEN_ARG);
            if !hidden {
                show_main_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let to_tray = window
                    .try_state::<AppState>()
                    .map(|s| s.engine.settings().close_to_tray)
                    .unwrap_or(true);
                if to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::pair_with,
            commands::pair_device,
            commands::respond_pairing,
            commands::disconnect_device,
            commands::connect_device,
            commands::forget_device,
            commands::set_blocked,
            commands::return_control,
            commands::get_layout,
            commands::save_layout,
            commands::get_settings,
            commands::save_settings,
            commands::get_diagnostics,
            commands::get_pairing_info,
            commands::open_receive_folder,
            commands::get_admin_mode,
            commands::set_admin_mode,
            commands::get_lock_service,
            commands::set_lock_service,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build MouseKey LAN");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            if let Some(state) = app.try_state::<AppState>() {
                state.engine.shutdown();
            }
        }
    });
}
