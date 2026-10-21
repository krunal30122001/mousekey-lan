use std::sync::Arc;

use mkl_core::Engine;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;

use crate::show_main_window;

pub fn create(app: &AppHandle, engine: Arc<Engine>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open MouseKey LAN", true, None::<&str>)?;
    let back = MenuItem::with_id(app, "return", "Return control to this PC  (Ctrl+Alt+Home)", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &back, &sep, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("MouseKey LAN")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "return" => engine.return_control(),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}
