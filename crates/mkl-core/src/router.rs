//! Hook-side routing: decides synchronously, inside the OS input hook, whether each physical
//! event stays on this machine or is forwarded to the device under control.
//!
//! While controlling a remote device the local cursor is parked at an anchor point and each
//! hooked move is converted to a delta, so movement is unaffected by local screen edges. The
//! remote cursor position is tracked here in the remote's own pixel coordinates.

use std::collections::HashSet;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mkl_input::{vk, CaptureSink, Disposition, HookEvent, SystemEvent};
use mkl_protocol::{InputEvent, Message, MouseButton, Rect};

use crate::layout::{anchor_point, clamp_to_screens, contains_any, edge_hit, edge_near, exit_dir, nearest};
use crate::state::{PeerConn, Shared};
use crate::views::EngineEvent;

const SWITCH_COOLDOWN: Duration = Duration::from_millis(250);

pub struct RemoteTarget {
    peer: Arc<PeerConn>,
    x: i32,
    y: i32,
    screens: Vec<Rect>,
}

#[derive(Default)]
pub enum Mode {
    #[default]
    Local,
    Remote(RemoteTarget),
}

#[derive(Default)]
pub struct RouterState {
    mode: Mode,
    anchor: (i32, i32),
    /// Every key physically held right now (for hotkey detection).
    held: HashSet<u16>,
    /// Keys the local OS has seen go down and not yet up.
    local_keys: HashSet<u16>,
    /// Keys held when control moved away; their key-ups must still reach the local OS.
    passthrough_ups: HashSet<u16>,
    remote_keys: HashSet<(u16, u16, bool)>,
    remote_buttons: HashSet<MouseButton>,
    local_buttons: HashSet<MouseButton>,
    last_switch: Option<Instant>,
    edge_since: Option<Instant>,
    /// Burst of physical mouse moves seen while another PC controls this one.
    local_motion: Option<(Instant, u32)>,
    /// The edge drop catcher is visible (left button held near an edge).
    catcher_shown: bool,
    /// Files reported by the drop catcher for the drag in progress.
    drag_files: Option<Vec<std::path::PathBuf>>,
    /// Files being carried to the remote PC; sent when the left button is released there.
    carrying: Option<Vec<std::path::PathBuf>>,
    /// Cursor move to perform once the router lock is released (see [`with_router`]).
    pending_cursor: Option<(i32, i32)>,
    /// The catcher is shown for a drag driven by the PC controlling this one.
    catcher_for_controller: bool,
}

/// Bridges the platform hook to the router.
pub struct HookSink(pub Arc<Shared>);

impl CaptureSink for HookSink {
    fn on_input(&self, ev: HookEvent) -> Disposition {
        let start = Instant::now();
        let d = handle_input(&self.0, ev);
        self.0.record_hook_time(start.elapsed());
        d
    }

    fn on_system(&self, ev: SystemEvent) {
        handle_system(&self.0, ev);
    }

    fn on_drag_files(&self, files: Vec<std::path::PathBuf>) {
        on_drag_files(&self.0, files);
    }

    fn on_drag_dropped(&self) {
        on_drag_dropped(&self.0);
    }
}

pub fn active_target(shared: &Shared) -> Option<String> {
    match &shared.router.lock().mode {
        Mode::Remote(t) => Some(t.peer.id.clone()),
        Mode::Local => None,
    }
}

/// Runs `f` with the router locked, then performs any cursor move it requested *after*
/// unlocking. `SetCursorPos` can feed back into the input hook, and the hook takes this lock:
/// moving the cursor while holding it could stall the hook until Windows removes it.
fn with_router<R>(shared: &Shared, f: impl FnOnce(&mut RouterState) -> R) -> R {
    let (result, cursor) = {
        let mut st = shared.router.lock();
        let r = f(&mut st);
        (r, st.pending_cursor.take())
    };
    if let Some((x, y)) = cursor {
        shared.injector.set_cursor_pos(x, y);
    }
    result
}

pub fn handle_input(shared: &Arc<Shared>, ev: HookEvent) -> Disposition {
    with_router(shared, |st| {
        if let HookEvent::Key { vk, down, .. } = ev {
            if down {
                st.held.insert(vk);
            } else {
                st.held.remove(&vk);
            }
        }
        if matches!(st.mode, Mode::Local) {
            local_input(shared, st, ev)
        } else {
            remote_input(shared, st, ev)
        }
    })
}

fn local_input(shared: &Arc<Shared>, st: &mut RouterState, ev: HookEvent) -> Disposition {
    match ev {
        HookEvent::Key { vk, down, .. } => {
            if down {
                st.local_keys.insert(vk);
            } else {
                st.local_keys.remove(&vk);
                st.passthrough_ups.remove(&vk);
            }
            // Typing on this PC's own keyboard while another PC drives the mouse is normal
            // (shared desk): keys simply work here and never take control away.
            Disposition::Pass
        }
        HookEvent::Button { button, down } => {
            if down {
                st.local_buttons.insert(button);
                reclaim_if_controlled(shared);
            } else {
                st.local_buttons.remove(&button);
                if button == MouseButton::Left {
                    hide_catcher(shared, st);
                }
            }
            Disposition::Pass
        }
        HookEvent::Wheel { .. } => Disposition::Pass,
        HookEvent::MouseMove { x, y } => {
            if deliberate_local_motion(shared, st) {
                reclaim_if_controlled(shared);
            }
            try_edge_switch(shared, st, x, y)
        }
    }
}

/// Distance from an edge at which the drop catcher appears during a drag.
const DRAG_MARGIN: i32 = 24;

fn hide_catcher(shared: &Shared, st: &mut RouterState) {
    if st.catcher_shown {
        shared.desktop.hide_drop_catcher();
        st.catcher_shown = false;
    }
    st.drag_files = None;
}

/// Left button held near an edge that leads to another PC: show the drop catcher so a file
/// drag can be detected; once files are reported and the cursor reaches the edge, carry them
/// to that PC.
fn drag_edge(shared: &Arc<Shared>, st: &mut RouterState, x: i32, y: i32, corner: i32) -> Disposition {
    let near = edge_near(&shared.local_screens.read(), x, y, corner, DRAG_MARGIN);
    let Some((dir, edge_pt)) = near else {
        hide_catcher(shared, st);
        return Disposition::Pass;
    };
    let screens = shared.screens_map();
    let target = shared.layout.read().resolve(&shared.me_id, &screens, edge_pt, dir);
    let Some((id, pt)) = target else {
        hide_catcher(shared, st);
        return Disposition::Pass;
    };
    if !st.catcher_shown {
        shared.desktop.show_drop_catcher(edge_pt.0, edge_pt.1);
        st.catcher_shown = true;
        return Disposition::Pass;
    }
    let at_edge = edge_hit(&shared.local_screens.read(), x, y, corner).is_some();
    if at_edge && st.drag_files.is_some() {
        let files = st.drag_files.take();
        hide_catcher(shared, st);
        if enter_remote(shared, st, &id, pt) {
            st.carrying = files;
            return Disposition::Swallow;
        }
    }
    Disposition::Pass
}

/// Files dragged onto the catcher (hook thread, outside the hook procedure).
pub fn on_drag_files(shared: &Shared, files: Vec<std::path::PathBuf>) {
    let mut st = shared.router.lock();
    if st.catcher_shown && matches!(st.mode, Mode::Local) && shared.settings.read().share_files {
        tracing::debug!(count = files.len(), "file drag detected at screen edge");
        st.drag_files = Some(files);
    }
}

/// Releases a carried drag on the remote PC: cancel the drag locally and send the files.
fn drop_carried(shared: &Arc<Shared>, st: &mut RouterState) {
    let Some(files) = st.carrying.take() else { return };
    st.local_buttons.remove(&MouseButton::Left);
    let Mode::Remote(t) = &st.mode else { return };
    let peer = t.peer.clone();
    let s = shared.clone();
    // Injection must not happen inside the hook procedure (SendInput waits for hooks).
    shared.rt.spawn(async move {
        let esc = |down| InputEvent::Key { vk: 0x1B, scan: 0x01, extended: false, down };
        let _ = s.injector.inject(&esc(true));
        let _ = s.injector.inject(&esc(false));
        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = s.injector.inject(&InputEvent::Button { button: MouseButton::Left, down: false });
        crate::transfer::send_files(&s, peer, mkl_protocol::TransferKind::DropFiles, files);
    });
}

fn try_edge_switch(shared: &Arc<Shared>, st: &mut RouterState, x: i32, y: i32) -> Disposition {
    let (enabled, corner, delay, share_files) = {
        let s = shared.settings.read();
        (s.edge_switching, if s.block_corners { s.corner_size } else { 0 }, s.switch_delay_ms, s.share_files)
    };
    let cooling = st.last_switch.is_some_and(|t| t.elapsed() < SWITCH_COOLDOWN);
    if enabled
        && share_files
        && !cooling
        && st.local_buttons.len() == 1
        && st.local_buttons.contains(&MouseButton::Left)
        && !shared.locked.load(Relaxed)
        && !shared.peers.read().is_empty()
    {
        return drag_edge(shared, st, x, y, corner);
    }
    if !enabled
        || !st.local_buttons.is_empty()
        || shared.locked.load(Relaxed)
        || st.last_switch.is_some_and(|t| t.elapsed() < SWITCH_COOLDOWN)
        || shared.peers.read().is_empty()
    {
        st.edge_since = None;
        return Disposition::Pass;
    }
    let hit = edge_hit(&shared.local_screens.read(), x, y, corner);
    let Some((dir, edge_pt)) = hit else {
        st.edge_since = None;
        return Disposition::Pass;
    };
    if delay > 0 {
        let since = *st.edge_since.get_or_insert_with(Instant::now);
        if since.elapsed() < Duration::from_millis(delay) {
            return Disposition::Pass;
        }
    }
    let screens = shared.screens_map();
    let target = shared.layout.read().resolve(&shared.me_id, &screens, edge_pt, dir);
    match target {
        Some((id, pt)) if enter_remote(shared, st, &id, pt) => Disposition::Swallow,
        _ => Disposition::Pass,
    }
}

fn enter_remote(shared: &Arc<Shared>, st: &mut RouterState, target: &str, pt: (i32, i32)) -> bool {
    let Some(peer) = shared.peer(target) else { return false };
    let screens = peer.screens.read().clone();
    if screens.is_empty() || peer.remote_locked.load(Relaxed) {
        return false;
    }
    let (x, y) = clamp_to_screens(&screens, pt.0, pt.1);
    if !peer.try_send(Message::ControlEnter { x, y }) {
        return false;
    }
    st.anchor = anchor_point(&shared.local_screens.read());
    st.pending_cursor = Some(st.anchor);
    // Only keys that are physically held right now: entries whose key-up the hook missed
    // (Win+L, Ctrl+Alt+Del, UAC) must not swallow releases meant for the remote.
    st.local_keys.retain(|vk| shared.injector.is_key_down(*vk));
    st.passthrough_ups = st.local_keys.clone();
    st.remote_keys.clear();
    st.remote_buttons.clear();
    st.mode = Mode::Remote(RemoteTarget { peer: peer.clone(), x, y, screens });
    st.last_switch = Some(Instant::now());
    st.edge_since = None;
    shared.stats.switches.fetch_add(1, Relaxed);
    tracing::debug!(target = %peer.id, x, y, "control moved to remote device");
    shared.emit(EngineEvent::ActiveChanged { active: Some(peer.id.clone()) });
    crate::transfer::on_control_entered(shared, &peer.id);
    true
}

fn switch_remote(shared: &Arc<Shared>, st: &mut RouterState, target: &str, pt: (i32, i32)) -> bool {
    let Some(peer) = shared.peer(target) else { return false };
    let screens = peer.screens.read().clone();
    if screens.is_empty() || peer.remote_locked.load(Relaxed) {
        return false;
    }
    let (x, y) = clamp_to_screens(&screens, pt.0, pt.1);
    if !peer.try_send(Message::ControlEnter { x, y }) {
        return false;
    }
    release_remote(st);
    st.mode = Mode::Remote(RemoteTarget { peer: peer.clone(), x, y, screens });
    st.last_switch = Some(Instant::now());
    shared.stats.switches.fetch_add(1, Relaxed);
    shared.emit(EngineEvent::ActiveChanged { active: Some(peer.id.clone()) });
    crate::transfer::on_control_entered(shared, &peer.id);
    true
}

/// Releases everything pressed on the current remote and tells it control is leaving.
fn release_remote(st: &mut RouterState) {
    if let Mode::Remote(t) = std::mem::take(&mut st.mode) {
        for (vk, scan, extended) in st.remote_keys.drain() {
            t.peer.try_send(Message::Input(InputEvent::Key { vk, scan, extended, down: false }));
        }
        for button in st.remote_buttons.drain() {
            t.peer.try_send(Message::Input(InputEvent::Button { button, down: false }));
        }
        t.peer.try_send(Message::ControlLeave);
    }
}

fn leave_remote(shared: &Shared, st: &mut RouterState, local_pt: Option<(i32, i32)>) {
    if matches!(st.mode, Mode::Local) {
        return;
    }
    release_remote(st);
    let (x, y) = local_pt.unwrap_or(st.anchor);
    st.pending_cursor = Some((x, y));
    st.last_switch = Some(Instant::now());
    st.carrying = None;
    st.edge_since = None;
    shared.stats.switches.fetch_add(1, Relaxed);
    tracing::debug!(x, y, "control returned to this device");
    shared.emit(EngineEvent::ActiveChanged { active: None });
}

fn remote_input(shared: &Arc<Shared>, st: &mut RouterState, ev: HookEvent) -> Disposition {
    let Mode::Remote(target) = &st.mode else { return Disposition::Pass };
    let peer = target.peer.clone();
    match ev {
        HookEvent::MouseMove { x, y } => {
            let dx = x - st.anchor.0;
            let dy = y - st.anchor.1;
            if dx == 0 && dy == 0 {
                return Disposition::Swallow;
            }
            remote_move(shared, st, dx, dy);
        }
        HookEvent::Button { button, down } => {
            if button == MouseButton::Left && !down && st.carrying.is_some() {
                drop_carried(shared, st);
                return Disposition::Swallow;
            }
            if down {
                st.remote_buttons.insert(button);
            } else {
                st.remote_buttons.remove(&button);
            }
            forward(shared, &peer, InputEvent::Button { button, down });
        }
        HookEvent::Wheel { delta, horizontal } => {
            forward(shared, &peer, InputEvent::Wheel { delta, horizontal });
        }
        HookEvent::Key { vk, scan, extended, down } => {
            if st.passthrough_ups.contains(&vk) {
                if down {
                    // Auto-repeat of a key held while crossing: it belongs to this PC.
                    return Disposition::Swallow;
                }
                st.passthrough_ups.remove(&vk);
                st.local_keys.remove(&vk);
                return Disposition::Pass;
            }
            if down
                && vk == vk::HOME
                && st.held.iter().any(|k| vk::is_ctrl(*k))
                && st.held.iter().any(|k| vk::is_alt(*k))
                && shared.settings.read().return_hotkey
            {
                leave_remote(shared, st, None);
                return Disposition::Swallow;
            }
            if down {
                st.remote_keys.insert((vk, scan, extended));
            } else {
                st.remote_keys.remove(&(vk, scan, extended));
            }
            forward(shared, &peer, InputEvent::Key { vk, scan, extended, down });
        }
    }
    Disposition::Swallow
}

fn remote_move(shared: &Arc<Shared>, st: &mut RouterState, dx: i32, dy: i32) {
    let Mode::Remote(t) = &mut st.mode else { return };
    let (nx, ny) = (t.x.saturating_add(dx), t.y.saturating_add(dy));
    if contains_any(&t.screens, nx, ny) {
        t.x = nx;
        t.y = ny;
        forward(shared, &t.peer, InputEvent::MouseMove { x: nx, y: ny });
        return;
    }

    // Leaving the remote desktop: find what lies beyond this edge.
    let Some(cur) = nearest(&t.screens, t.x, t.y) else { return };
    let dir = exit_dir(&cur, nx, ny);
    let edge_pt = cur.clamp(nx, ny);
    let peer_id = t.peer.id.clone();
    if shared.settings.read().edge_switching
        && st.last_switch.map_or(true, |s| s.elapsed() >= SWITCH_COOLDOWN)
    {
        let screens = shared.screens_map();
        let next = shared.layout.read().resolve(&peer_id, &screens, edge_pt, dir);
        match next {
            Some((id, pt)) if id == shared.me_id => {
                leave_remote(shared, st, Some(pt));
                return;
            }
            Some((id, pt)) => {
                if switch_remote(shared, st, &id, pt) {
                    return;
                }
            }
            None => {}
        }
    }

    let Mode::Remote(t) = &mut st.mode else { return };
    let (cx, cy) = clamp_to_screens(&t.screens, nx, ny);
    if (cx, cy) != (t.x, t.y) {
        t.x = cx;
        t.y = cy;
        forward(shared, &t.peer, InputEvent::MouseMove { x: cx, y: cy });
    }
}

fn forward(shared: &Shared, peer: &PeerConn, ev: InputEvent) {
    if peer.try_send(Message::Input(ev)) {
        shared.stats.forwarded.fetch_add(1, Relaxed);
    }
}

/// Physical input on a machine that is being controlled hands control back to its own user.
/// Releases happen off the hook thread so the hook returns immediately.
/// Physical mouse motion on a controlled PC counts as "the local user wants control back"
/// only when it is sustained (several moves in quick succession), not a bump or sensor jitter.
fn deliberate_local_motion(shared: &Shared, st: &mut RouterState) -> bool {
    const WINDOW: Duration = Duration::from_millis(400);
    const MOVES: u32 = 8;
    if !shared.being_controlled.load(Relaxed) {
        st.local_motion = None;
        return false;
    }
    let now = Instant::now();
    match &mut st.local_motion {
        Some((start, count)) if now.duration_since(*start) <= WINDOW => {
            *count += 1;
            if *count >= MOVES {
                st.local_motion = None;
                return true;
            }
        }
        slot => *slot = Some((now, 1)),
    }
    false
}

fn reclaim_if_controlled(shared: &Arc<Shared>) {
    if !shared.being_controlled.load(Relaxed) || !shared.settings.read().local_input_reclaims {
        return;
    }
    let s = shared.clone();
    shared.rt.spawn(async move {
        let controller = {
            let mut inj = s.inject.lock();
            let Some(c) = inj.controlled_by.clone() else { return };
            inj.end(&c, &*s.injector);
            c
        };
        if let Some(p) = s.peer(&controller) {
            p.try_send(Message::ControlRevoked);
        }
        tracing::debug!(%controller, "local input reclaimed control");
        s.changed();
    });
}

/// Returns control to this machine (tray action, hotkey, lock, shutdown...).
pub fn return_local(shared: &Shared) {
    with_router(shared, |st| leave_remote(shared, st, None));
}

/// A session ended or was replaced: stop driving it.
pub fn on_peer_gone(shared: &Shared, conn_id: u64) {
    with_router(shared, |st| {
        if matches!(&st.mode, Mode::Remote(t) if t.peer.conn_id == conn_id) {
            leave_remote(shared, st, None);
        }
    });
}

/// The controlled device's user took over, or it locked: stop driving it.
pub fn on_remote_unavailable(shared: &Shared, peer_id: &str) {
    with_router(shared, |st| {
        if matches!(&st.mode, Mode::Remote(t) if t.peer.id == peer_id) {
            leave_remote(shared, st, None);
        }
    });
}

/// Called after input from the controlling PC was injected. If that PC is dragging something
/// here (its left button is held through us) towards an edge that leads back to it, show the
/// drop catcher so the drag's files can be picked up and carried to the controller.
pub fn on_injected_move(shared: &Shared, controller: &str, x: i32, y: i32, left_held: bool) {
    let (share_files, corner) = {
        let s = shared.settings.read();
        (s.share_files, if s.block_corners { s.corner_size } else { 0 })
    };
    let mut st = shared.router.lock();
    if !matches!(st.mode, Mode::Local) || (st.catcher_shown && !st.catcher_for_controller) {
        return;
    }
    let target = if share_files && left_held {
        let near = edge_near(&shared.local_screens.read(), x, y, corner, DRAG_MARGIN);
        near.and_then(|(dir, pt)| {
            let screens = shared.screens_map();
            let r = shared.layout.read().resolve(&shared.me_id, &screens, pt, dir);
            r.filter(|(id, _)| id == controller).map(|_| pt)
        })
    } else {
        None
    };
    match target {
        Some(pt) if !st.catcher_shown => {
            shared.desktop.show_drop_catcher(pt.0, pt.1);
            st.catcher_shown = true;
            st.catcher_for_controller = true;
        }
        Some(_) => {}
        // Moved away from the edge, or the drag ended elsewhere on this PC.
        None if st.catcher_shown => {
            shared.desktop.hide_drop_catcher();
            st.catcher_shown = false;
            st.catcher_for_controller = false;
            st.drag_files = None;
        }
        None => {}
    }
}

/// Control left this PC: once any pending drop has landed, remove a catcher that was shown
/// for the controller so it can't intercept this PC's own clicks at the edge.
pub fn hide_controller_catcher_later(shared: &Arc<Shared>) {
    let s = shared.clone();
    shared.rt.spawn(async move {
        tokio::time::sleep(Duration::from_millis(800)).await;
        let mut st = s.router.lock();
        if st.catcher_shown && st.catcher_for_controller {
            s.desktop.hide_drop_catcher();
            st.catcher_shown = false;
            st.catcher_for_controller = false;
            st.drag_files = None;
        }
    });
}

/// A drag was released on the drop catcher. If it was driven by the controlling PC (it just
/// moved back across the edge, releasing our left button), send the files to it.
pub fn on_drag_dropped(shared: &Arc<Shared>) {
    let files = {
        let mut st = shared.router.lock();
        if !st.catcher_for_controller {
            return;
        }
        shared.desktop.hide_drop_catcher();
        st.catcher_shown = false;
        st.catcher_for_controller = false;
        st.drag_files.take()
    };
    let controller = shared.inject.lock().controlled_by.clone().or_else(|| shared.last_controller.lock().clone());
    if let (Some(files), Some(controller)) = (files, controller) {
        if let Some(peer) = shared.peer(&controller) {
            tracing::info!(count = files.len(), to = %controller, "dragged files carried back to the controlling PC");
            crate::transfer::send_files(shared, peer, mkl_protocol::TransferKind::DropFiles, files);
        }
    }
}

pub fn handle_system(shared: &Arc<Shared>, ev: SystemEvent) {
    if ev == SystemEvent::ClipboardChanged {
        tracing::debug!("clipboard changed");
    } else {
        tracing::info!(?ev, "system event");
    }
    match ev {
        SystemEvent::ClipboardChanged => {
            crate::transfer::on_clipboard_changed(shared);
            return;
        }
        SystemEvent::DisplayChanged => {
            let screens = shared.injector.screens();
            *shared.local_screens.write() = screens.clone();
            shared.broadcast(Message::Screens(screens));
            shared.changed();
        }
        SystemEvent::SessionLocked | SystemEvent::Suspend => {
            if ev == SystemEvent::SessionLocked {
                shared.locked.store(true, Relaxed);
                shared.refresh_lock_status();
            }
            return_local(shared);
            {
                let mut st = shared.router.lock();
                st.local_keys.clear();
                st.passthrough_ups.clear();
                st.held.clear();
            }
            // With the lock-screen service the PC driving this one keeps control, so it can
            // type the password; anything it held down is still released.
            let keep_control = ev == SystemEvent::SessionLocked && !shared.input_blocked();
            let s = shared.clone();
            shared.rt.spawn(async move {
                let mut inj = s.inject.lock();
                if keep_control {
                    inj.release_all(&*s.injector);
                } else if let Some(c) = inj.controlled_by.clone() {
                    inj.end(&c, &*s.injector);
                }
            });
            shared.changed();
        }
        SystemEvent::SessionUnlocked | SystemEvent::Resume => {
            if ev == SystemEvent::SessionUnlocked {
                shared.locked.store(false, Relaxed);
                shared.refresh_lock_status();
            }
            *shared.local_screens.write() = shared.injector.screens();
            shared.backoff.lock().clear();
            shared.reconnect_now.notify_one();
            shared.changed();
        }
    }
}
