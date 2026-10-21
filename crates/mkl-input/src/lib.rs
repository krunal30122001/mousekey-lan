//! Input capture and injection.
//!
//! The rest of the application only sees the [`CaptureSink`] / [`InputInjector`] traits; each
//! operating system provides its own implementation module. Windows is implemented with
//! low-level hooks (`WH_MOUSE_LL` / `WH_KEYBOARD_LL`) and `SendInput`. Other platforms
//! currently compile to an "unsupported" backend so the workspace stays portable.

use std::sync::Arc;

pub use mkl_protocol::{InputEvent, MouseButton, Rect};

/// A physical input event observed by the global hook (never includes injected input).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// Position the cursor would move to, in physical virtual-desktop pixels.
    MouseMove { x: i32, y: i32 },
    Button { button: MouseButton, down: bool },
    Wheel { delta: i32, horizontal: bool },
    Key { vk: u16, scan: u16, extended: bool, down: bool },
}

/// Whether the local OS should still receive a hooked event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Pass,
    Swallow,
}

/// Session / power / display notifications delivered on the hook thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEvent {
    SessionLocked,
    SessionUnlocked,
    DisplayChanged,
    ClipboardChanged,
    Suspend,
    Resume,
}

/// Receives hooked input. Called synchronously on the hook thread, so implementations must
/// return quickly (the OS removes hooks that stall).
pub trait CaptureSink: Send + Sync {
    fn on_input(&self, ev: HookEvent) -> Disposition;
    fn on_system(&self, ev: SystemEvent);
    /// Files are being dragged over the edge drop catcher (see [`Desktop::show_drop_catcher`]).
    fn on_drag_files(&self, _files: Vec<std::path::PathBuf>) {}
    /// A drag was released on the drop catcher.
    fn on_drag_dropped(&self) {}
}

/// Clipboard contents that can be shared between devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipContent {
    Text(String),
    /// Device-independent bitmap (`CF_DIB` layout: BITMAPINFOHEADER + pixels).
    Image(Vec<u8>),
    Files(Vec<std::path::PathBuf>),
}

/// Desktop integration besides raw input: clipboard, drag detection, file manager.
pub trait Desktop: Send + Sync {
    /// Changes every time the clipboard content changes.
    fn clipboard_sequence(&self) -> u32;
    fn read_clipboard(&self) -> Option<ClipContent>;
    fn write_clipboard(&self, content: &ClipContent) -> Result<(), InputError>;
    /// Shows an invisible drop target under `(x, y)` so a file drag reaching the screen edge
    /// can be detected (reported through [`CaptureSink::on_drag_files`]).
    fn show_drop_catcher(&self, x: i32, y: i32);
    fn hide_drop_catcher(&self);
    /// Opens the file manager with `path` selected.
    fn reveal(&self, path: &std::path::Path);
    /// This process runs with administrator rights.
    fn is_elevated(&self) -> bool {
        false
    }
    /// The window that receives keyboard input belongs to an elevated (admin) process, so a
    /// non-elevated process can't type into it (Windows UIPI).
    fn foreground_is_elevated(&self) -> bool {
        false
    }
}

/// Synthesizes input on this machine.
pub trait InputInjector: Send + Sync {
    fn inject(&self, ev: &InputEvent) -> Result<(), InputError>;
    /// Monitor rectangles in physical pixels; the primary monitor comes first.
    fn screens(&self) -> Vec<Rect>;
    fn set_cursor_pos(&self, x: i32, y: i32);
    fn cursor_pos(&self) -> (i32, i32);
    /// Whether a key is physically held right now (used to discard stale key state).
    fn is_key_down(&self, _vk: u16) -> bool {
        true
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("input capture is not supported on this platform yet")]
    Unsupported,
    #[error("failed to install global input hooks (os error {0})")]
    HookInstall(u32),
    #[error("input injection was blocked (os error {0}); the target desktop may be locked or elevated")]
    Inject(u32),
    #[error("input thread failed: {0}")]
    Thread(String),
}

/// Virtual-key codes the engine needs to recognise.
pub mod vk {
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const HOME: u16 = 0x24;
    pub const LWIN: u16 = 0x5B;
    pub const RWIN: u16 = 0x5C;
    pub const LSHIFT: u16 = 0xA0;
    pub const RSHIFT: u16 = 0xA1;
    pub const LCONTROL: u16 = 0xA2;
    pub const RCONTROL: u16 = 0xA3;
    pub const LMENU: u16 = 0xA4;
    pub const RMENU: u16 = 0xA5;

    pub fn is_ctrl(vk: u16) -> bool {
        matches!(vk, CONTROL | LCONTROL | RCONTROL)
    }
    pub fn is_alt(vk: u16) -> bool {
        matches!(vk, MENU | LMENU | RMENU)
    }
}

#[cfg(windows)]
mod win;
#[cfg(windows)]
mod win_desktop;
#[cfg(windows)]
pub use win::{init_process_dpi, start_capture, CaptureHandle, WinInjector as PlatformInjector};
#[cfg(windows)]
pub use win_desktop::WinDesktop as PlatformDesktop;

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use unsupported::{
    init_process_dpi, start_capture, CaptureHandle, NullDesktop as PlatformDesktop,
    NullInjector as PlatformInjector,
};

/// Injector for the current platform.
pub fn platform_injector() -> Arc<dyn InputInjector> {
    Arc::new(PlatformInjector::default())
}

/// Clipboard / drag-and-drop integration for the current platform.
pub fn platform_desktop() -> Arc<dyn Desktop> {
    Arc::new(PlatformDesktop::default())
}

/// A desktop that does nothing (tests, headless use).
#[derive(Default)]
pub struct NoDesktop;

impl Desktop for NoDesktop {
    fn clipboard_sequence(&self) -> u32 {
        0
    }
    fn read_clipboard(&self) -> Option<ClipContent> {
        None
    }
    fn write_clipboard(&self, _content: &ClipContent) -> Result<(), InputError> {
        Ok(())
    }
    fn show_drop_catcher(&self, _x: i32, _y: i32) {}
    fn hide_drop_catcher(&self) {}
    fn reveal(&self, _path: &std::path::Path) {}
}
