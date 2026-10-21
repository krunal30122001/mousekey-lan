//! Placeholder backend for platforms without an implementation yet (macOS, Linux).
//! macOS will need CGEventTap + CGEventPost (Accessibility permission); Linux will need
//! libei / uinput depending on the compositor. See docs/PLATFORM.md.

use std::sync::Arc;

use crate::{CaptureSink, InputError, InputEvent, InputInjector, Rect};

pub struct CaptureHandle;

pub fn init_process_dpi() {}

pub fn start_capture(_sink: Arc<dyn CaptureSink>) -> Result<CaptureHandle, InputError> {
    Err(InputError::Unsupported)
}

pub type NullDesktop = crate::NoDesktop;

#[derive(Default)]
pub struct NullInjector;

impl InputInjector for NullInjector {
    fn inject(&self, _ev: &InputEvent) -> Result<(), InputError> {
        Err(InputError::Unsupported)
    }
    fn screens(&self) -> Vec<Rect> {
        vec![Rect::new(0, 0, 1920, 1080)]
    }
    fn set_cursor_pos(&self, _x: i32, _y: i32) {}
    fn cursor_pos(&self) -> (i32, i32) {
        (0, 0)
    }
}
