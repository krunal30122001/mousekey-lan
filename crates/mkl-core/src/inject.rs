//! Receiving side: validates incoming input and injects it, tracking everything pressed so
//! it can always be released (no stuck keys after disconnects, sleep or control changes).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mkl_input::{InputError, InputInjector};
use mkl_protocol::{InputEvent, MouseButton, Rect};

use crate::layout::clamp_to_screens;

const MAX_WHEEL: i32 = 120 * 40;

#[derive(Default)]
pub struct InjectState {
    /// Device currently allowed to drive this machine.
    pub controlled_by: Option<String>,
    keys: HashSet<(u16, u16, bool)>,
    buttons: HashSet<MouseButton>,
    /// Lock-free mirror of `controlled_by.is_some()` for the input hook. The hook must never
    /// wait on this struct's mutex: `SendInput` (called while holding it) waits for the hook.
    active: Arc<AtomicBool>,
}

impl InjectState {
    pub fn active_flag(&self) -> Arc<AtomicBool> {
        self.active.clone()
    }

    pub fn begin(&mut self, peer: &str, injector: &dyn InputInjector) {
        if self.controlled_by.as_deref() != Some(peer) {
            self.release_all(injector);
        }
        self.controlled_by = Some(peer.to_string());
        self.active.store(true, Ordering::Release);
    }

    /// Ends control by `peer` (no-op if someone else is in control).
    pub fn end(&mut self, peer: &str, injector: &dyn InputInjector) {
        if self.controlled_by.as_deref() == Some(peer) {
            self.release_all(injector);
            self.controlled_by = None;
            self.active.store(false, Ordering::Release);
        }
    }

    /// Injects `ev` if `peer` is the current controller. Returns `Ok(false)` when the event
    /// was refused (not the controller, or invalid).
    pub fn apply(
        &mut self,
        peer: &str,
        ev: InputEvent,
        injector: &dyn InputInjector,
        screens: &[Rect],
    ) -> Result<bool, InputError> {
        if self.controlled_by.as_deref() != Some(peer) {
            return Ok(false);
        }
        let Some(ev) = sanitize(ev, screens) else { return Ok(false) };
        match ev {
            InputEvent::Key { vk, scan, extended, down } => {
                // A release for a key we never pressed is still injected: it clears keys
                // that were pressed before control arrived.
                if down {
                    self.keys.insert((vk, scan, extended));
                } else {
                    self.keys.remove(&(vk, scan, extended));
                }
            }
            InputEvent::Button { button, down } => {
                if down {
                    self.buttons.insert(button);
                } else {
                    self.buttons.remove(&button);
                }
            }
            _ => {}
        }
        injector.inject(&ev)?;
        Ok(true)
    }

    pub fn release_all(&mut self, injector: &dyn InputInjector) {
        for (vk, scan, extended) in self.keys.drain() {
            let _ = injector.inject(&InputEvent::Key { vk, scan, extended, down: false });
        }
        for button in self.buttons.drain() {
            let _ = injector.inject(&InputEvent::Button { button, down: false });
        }
    }

    pub fn is_down(&self, button: MouseButton) -> bool {
        self.buttons.contains(&button)
    }

    pub fn pressed_count(&self) -> usize {
        self.keys.len() + self.buttons.len()
    }
}

fn sanitize(ev: InputEvent, screens: &[Rect]) -> Option<InputEvent> {
    match ev {
        InputEvent::MouseMove { x, y } => {
            let (x, y) = clamp_to_screens(screens, x, y);
            Some(InputEvent::MouseMove { x, y })
        }
        InputEvent::Wheel { delta, horizontal } => {
            Some(InputEvent::Wheel { delta: delta.clamp(-MAX_WHEEL, MAX_WHEEL), horizontal })
        }
        InputEvent::Key { vk, scan, .. } if (vk == 0 && scan == 0) || scan > 0x1FF || vk > 0xFF => None,
        other => Some(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct Rec(Mutex<Vec<InputEvent>>);
    impl InputInjector for Rec {
        fn inject(&self, ev: &InputEvent) -> Result<(), InputError> {
            self.0.lock().push(*ev);
            Ok(())
        }
        fn screens(&self) -> Vec<Rect> {
            vec![Rect::new(0, 0, 100, 100)]
        }
        fn set_cursor_pos(&self, _: i32, _: i32) {}
        fn cursor_pos(&self) -> (i32, i32) {
            (0, 0)
        }
    }

    #[test]
    fn only_controller_may_inject_and_everything_is_released() {
        let rec = Rec::default();
        let screens = rec.screens();
        let mut st = InjectState::default();
        let key = InputEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down: true };
        assert_eq!(st.apply("x", key, &rec, &screens).unwrap(), false);
        st.begin("a", &rec);
        assert!(st.apply("a", key, &rec, &screens).unwrap());
        assert!(st.apply("a", InputEvent::Button { button: MouseButton::Left, down: true }, &rec, &screens).unwrap());
        assert!(!st.apply("b", key, &rec, &screens).unwrap());
        assert!(st.apply("a", InputEvent::MouseMove { x: 500, y: -4 }, &rec, &screens).unwrap());
        st.end("a", &rec);
        let log = rec.0.lock().clone();
        assert!(log.contains(&InputEvent::MouseMove { x: 99, y: 0 }));
        assert!(log.contains(&InputEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down: false }));
        assert!(log.contains(&InputEvent::Button { button: MouseButton::Left, down: false }));
        assert_eq!(st.pressed_count(), 0);
        assert!(st.controlled_by.is_none());
    }

    #[test]
    fn invalid_keys_are_refused() {
        let rec = Rec::default();
        let mut st = InjectState::default();
        st.begin("a", &rec);
        let bad = InputEvent::Key { vk: 0, scan: 0, extended: false, down: true };
        assert!(!st.apply("a", bad, &rec, &rec.screens()).unwrap());
    }
}
