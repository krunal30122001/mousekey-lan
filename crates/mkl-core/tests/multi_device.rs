//! Multi-device network tests: real engines talking over localhost TCP with mock injectors.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mkl_core::{DeviceStatus, Engine, EngineConfig, EngineEvent, LayoutPos};
use mkl_input::{ClipContent, Desktop, Disposition, HookEvent, InputError, InputEvent, InputInjector, MouseButton, Rect, SystemEvent};
use mkl_store::Store;
use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Default)]
struct MockInjector {
    events: Mutex<Vec<InputEvent>>,
    cursor: Mutex<(i32, i32)>,
}

impl InputInjector for MockInjector {
    fn inject(&self, ev: &InputEvent) -> Result<(), InputError> {
        if let InputEvent::MouseMove { x, y } = *ev {
            *self.cursor.lock() = (x, y);
        }
        self.events.lock().push(*ev);
        Ok(())
    }
    fn screens(&self) -> Vec<Rect> {
        vec![Rect::new(0, 0, 1920, 1080)]
    }
    fn set_cursor_pos(&self, x: i32, y: i32) {
        *self.cursor.lock() = (x, y);
    }
    fn cursor_pos(&self) -> (i32, i32) {
        *self.cursor.lock()
    }
    fn is_key_down(&self, _vk: u16) -> bool {
        false
    }
}

/// In-memory clipboard; records drop-catcher and reveal calls.
#[derive(Default)]
struct MockDesktop {
    clip: Mutex<Option<ClipContent>>,
    seq: AtomicU32,
    catcher: Mutex<Option<(i32, i32)>>,
    revealed: Mutex<Vec<PathBuf>>,
}

impl MockDesktop {
    fn set(&self, c: ClipContent) {
        *self.clip.lock() = Some(c);
        self.seq.fetch_add(1, Ordering::SeqCst);
    }
}

impl Desktop for MockDesktop {
    fn clipboard_sequence(&self) -> u32 {
        self.seq.load(Ordering::SeqCst)
    }
    fn read_clipboard(&self) -> Option<ClipContent> {
        self.clip.lock().clone()
    }
    fn write_clipboard(&self, c: &ClipContent) -> Result<(), InputError> {
        self.set(c.clone());
        Ok(())
    }
    fn show_drop_catcher(&self, x: i32, y: i32) {
        *self.catcher.lock() = Some((x, y));
    }
    fn hide_drop_catcher(&self) {
        *self.catcher.lock() = None;
    }
    fn reveal(&self, path: &Path) {
        self.revealed.lock().push(path.to_path_buf());
    }
}

struct Node {
    engine: Engine,
    events: UnboundedReceiver<EngineEvent>,
    injector: Arc<MockInjector>,
    desktop: Arc<MockDesktop>,
}

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mkl-test-{}-{}-{tag}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn node() -> Node {
    let injector = Arc::new(MockInjector::default());
    let desktop = Arc::new(MockDesktop::default());
    let (engine, events) = Engine::start(EngineConfig {
        store: Arc::new(Store::open_in_memory().unwrap()),
        injector: injector.clone(),
        listen_port: Some(0),
        enable_capture: false,
        enable_discovery: false,
        desktop: desktop.clone(),
        receive_dir: scratch("recv"),
        temp_dir: scratch("tmp"),
    })
    .await
    .unwrap();
    Node { engine, events, injector, desktop }
}

fn addr(n: &Node) -> String {
    format!("127.0.0.1:{}", n.engine.listen_port())
}

async fn wait_for<F: Fn() -> bool>(what: &str, f: F) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn next_event<F: Fn(&EngineEvent) -> bool>(rx: &mut UnboundedReceiver<EngineEvent>, f: F) -> EngineEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ev = rx.recv().await.expect("event channel closed");
            if f(&ev) {
                return ev;
            }
        }
    })
    .await
    .expect("expected engine event")
}

fn online(n: &Node, id: &str) -> bool {
    n.engine.snapshot().devices.iter().any(|d| d.id == id && d.status == DeviceStatus::Online)
}

async fn paired_pair() -> (Node, Node) {
    let a = node().await;
    let mut b = node().await;
    a.engine.pair_with(&addr(&b)).unwrap();
    let req = next_event(&mut b.events, |e| matches!(e, EngineEvent::PairRequest { .. })).await;
    let EngineEvent::PairRequest { request_id, device_id, code, .. } = req else { unreachable!() };
    assert_eq!(device_id, a.engine.device_id());
    assert_eq!(code.len(), 7);
    b.engine.respond_pairing(request_id, true, false).unwrap();
    let (aid, bid) = (a.engine.device_id().to_string(), b.engine.device_id().to_string());
    wait_for("both sides online", || online(&a, &bid) && online(&b, &aid)).await;

    // Arrange A | B on A; the layout must propagate to B.
    a.engine
        .save_layout(vec![
            LayoutPos { id: aid.clone(), x: 0, y: 0 },
            LayoutPos { id: bid.clone(), x: 1920, y: 0 },
        ])
        .unwrap();
    wait_for("layout synced to B", || {
        b.engine.layout_view().devices.iter().any(|d| d.id == aid && d.x == 0)
            && b.engine.layout_view().devices.iter().any(|d| d.id == bid && d.x == 1920)
    })
    .await;
    (a, b)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_and_input_forwarding() {
    let (a, b) = paired_pair().await;

    // Default layout places B to the right of A. Push A's cursor against its right edge.
    let d = a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 });
    assert_eq!(d, Disposition::Swallow, "edge hit should move control to B");
    assert_eq!(a.engine.snapshot().active_target.as_deref(), Some(b.engine.device_id()));
    // A parks its cursor at its anchor (centre of primary screen).
    assert_eq!(*a.injector.cursor.lock(), (960, 540));

    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 970, y: 545 }), Disposition::Swallow);
    let key_down = HookEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down: true };
    let key_up = HookEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down: false };
    assert_eq!(a.engine.feed_input(key_down), Disposition::Swallow);
    assert_eq!(a.engine.feed_input(key_up), Disposition::Swallow);

    wait_for("B to inject input", || b.injector.events.lock().len() >= 4).await;
    let got = b.injector.events.lock().clone();
    assert_eq!(got[0], InputEvent::MouseMove { x: 2, y: 500 });
    assert_eq!(got[1], InputEvent::MouseMove { x: 12, y: 505 });
    assert_eq!(got[2], InputEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down: true });
    assert_eq!(got[3], InputEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down: false });
    assert_eq!(b.engine.snapshot().controlled_by.as_deref(), Some(a.engine.device_id()));

    // Moving back off B's left edge returns control to A, near A's right edge.
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 960 - 40, y: 540 }), Disposition::Swallow);
    // (cooldown) wait, then try again
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.engine.feed_input(HookEvent::MouseMove { x: 960 - 40, y: 540 });
    assert!(a.engine.snapshot().active_target.is_none(), "control should be back on A");
    assert_eq!(a.injector.cursor.lock().0, 1917);
    wait_for("B released control", || b.engine.snapshot().controlled_by.is_none()).await;

    let diag = a.engine.diagnostics();
    assert!(diag.forwarded >= 3);
    assert_eq!(diag.peers.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_keys_are_released_when_connection_drops() {
    let (a, b) = paired_pair().await;
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    a.engine.feed_input(HookEvent::Key { vk: 0x10, scan: 0x2A, extended: false, down: true });
    wait_for("shift down on B", || {
        b.injector.events.lock().contains(&InputEvent::Key { vk: 0x10, scan: 0x2A, extended: false, down: true })
    })
    .await;
    // Drop the connection abruptly from B's side.
    b.engine.disconnect(a.engine.device_id());
    wait_for("shift released on B", || {
        b.injector.events.lock().contains(&InputEvent::Key { vk: 0x10, scan: 0x2A, extended: false, down: false })
    })
    .await;
    wait_for("A back to local", || a.engine.snapshot().active_target.is_none()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trusted_devices_reconnect_automatically() {
    let (a, b) = paired_pair().await;
    let bid = b.engine.device_id().to_string();
    b.engine.disconnect(a.engine.device_id());
    wait_for("disconnected", || !online(&a, &bid)).await;
    b.engine.connect(a.engine.device_id());
    wait_for("reconnected", || online(&a, &bid)).await;
    assert!(a.engine.diagnostics().reconnects >= 1 || b.engine.diagnostics().reconnects >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declined_and_blocked_devices_cannot_connect() {
    let mut a = node().await;
    let mut b = node().await;
    a.engine.pair_with(&addr(&b)).unwrap();
    let EngineEvent::PairRequest { request_id, .. } =
        next_event(&mut b.events, |e| matches!(e, EngineEvent::PairRequest { .. })).await
    else {
        unreachable!()
    };
    b.engine.respond_pairing(request_id, false, true).unwrap();
    let res = next_event(&mut a.events, |e| matches!(e, EngineEvent::PairResult { .. })).await;
    assert!(matches!(res, EngineEvent::PairResult { success: false, .. }));
    let snap = b.engine.snapshot();
    assert!(snap.devices.iter().any(|d| d.id == a.engine.device_id() && d.blocked));

    // Blocked: a second attempt is refused before any prompt is shown.
    a.engine.pair_with(&addr(&b)).unwrap();
    let res = next_event(&mut a.events, |e| matches!(e, EngineEvent::PairResult { .. })).await;
    assert!(matches!(res, EngineEvent::PairResult { success: false, .. }));
    tokio::time::sleep(Duration::from_millis(200)).await;
    while let Ok(ev) = b.events.try_recv() {
        assert!(!matches!(ev, EngineEvent::PairRequest { .. }), "blocked device must not trigger a prompt");
    }
    assert!(b.injector.events.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identity_pinning_rejects_wrong_device() {
    let mut a = node().await;
    let b = node().await;
    // A pairing link that names a different device id must be refused after the handshake.
    a.engine.pair_with(&format!("mkl://127.0.0.1:{}?id=00000000deadbeef", b.engine.listen_port())).unwrap();
    let res = next_event(&mut a.events, |e| matches!(e, EngineEvent::PairResult { .. })).await;
    let EngineEvent::PairResult { success, message, .. } = res else { unreachable!() };
    assert!(!success);
    assert!(message.contains("identity mismatch"), "{message}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forgotten_device_is_rejected_and_revoked() {
    let (a, b) = paired_pair().await;
    let (aid, bid) = (a.engine.device_id().to_string(), b.engine.device_id().to_string());
    b.engine.forget(&aid).unwrap();
    // A will try to reconnect; B no longer trusts it, so A drops its trust as well.
    wait_for("A revokes B", || {
        a.engine.snapshot().devices.iter().any(|d| d.id == bid && !d.trusted)
    })
    .await;
    assert!(!online(&b, &aid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_devices_chain_switching() {
    let (a, b) = paired_pair().await;
    let mut c = node().await;
    a.engine.pair_with(&addr(&c)).unwrap();
    let EngineEvent::PairRequest { request_id, .. } =
        next_event(&mut c.events, |e| matches!(e, EngineEvent::PairRequest { .. })).await
    else {
        unreachable!()
    };
    c.engine.respond_pairing(request_id, true, false).unwrap();
    let cid = c.engine.device_id().to_string();
    wait_for("C online on A", || online(&a, &cid)).await;

    // Layout on A: A | B | C.
    let layout = a.engine.layout_view();
    let c_pos = layout.devices.iter().find(|d| d.id == cid).unwrap();
    assert_eq!(c_pos.x, 3840);

    a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 300 });
    assert_eq!(a.engine.snapshot().active_target.as_deref(), Some(b.engine.device_id()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Travel across B (1920 px wide) in one big delta: crosses into C.
    a.engine.feed_input(HookEvent::MouseMove { x: 960 + 1950, y: 540 });
    assert_eq!(a.engine.snapshot().active_target.as_deref(), Some(cid.as_str()));
    wait_for("C injected", || !c.injector.events.lock().is_empty()).await;
    wait_for("B released", || b.engine.snapshot().controlled_by.is_none()).await;
}

/// Regression: a key whose release the hook missed (Win+L, Ctrl+Alt+Del, UAC) must not
/// leave that key stuck down on the remote PC.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_local_key_does_not_stick_on_remote() {
    let (a, b) = paired_pair().await;
    let ctrl_down = HookEvent::Key { vk: 0xA2, scan: 0x1D, extended: false, down: true };
    let ctrl_up = HookEvent::Key { vk: 0xA2, scan: 0x1D, extended: false, down: false };
    // Ctrl goes down locally; its release is never seen (e.g. the PC was locked).
    assert_eq!(a.engine.feed_input(ctrl_down), Disposition::Pass);
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    // On the remote, Ctrl is pressed and released normally: both must reach B.
    assert_eq!(a.engine.feed_input(ctrl_down), Disposition::Swallow);
    assert_eq!(a.engine.feed_input(ctrl_up), Disposition::Swallow);
    wait_for("ctrl down+up on B", || {
        let ev = b.injector.events.lock();
        ev.contains(&InputEvent::Key { vk: 0xA2, scan: 0x1D, extended: false, down: true })
            && ev.contains(&InputEvent::Key { vk: 0xA2, scan: 0x1D, extended: false, down: false })
    })
    .await;
    assert_eq!(b.engine.diagnostics().keys_held_by_remote, 0);
}

/// Regression: typing on the controlled PC's own keyboard (or bumping its mouse) must not
/// bounce control back; only a deliberate local mouse movement reclaims it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_typing_on_controlled_pc_keeps_control() {
    let (a, b) = paired_pair().await;
    let bid = b.engine.device_id().to_string();
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    wait_for("B controlled", || b.engine.snapshot().controlled_by.is_some()).await;

    // B's own keyboard types locally and leaves A in control.
    for down in [true, false, true, false] {
        let d = b.engine.feed_input(HookEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down });
        assert_eq!(d, Disposition::Pass);
    }
    // A small bump of B's mouse doesn't count either.
    for i in 0..3 {
        b.engine.feed_input(HookEvent::MouseMove { x: 100 + i, y: 100 });
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.engine.snapshot().active_target.as_deref(), Some(bid.as_str()));

    // A deliberate movement of B's mouse takes control back.
    for i in 0..10 {
        b.engine.feed_input(HookEvent::MouseMove { x: 200 + i * 5, y: 100 });
    }
    wait_for("A returned to local", || a.engine.snapshot().active_target.is_none()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clipboard_text_and_image_sync_both_ways_without_loops() {
    let (a, b) = paired_pair().await;
    a.desktop.set(ClipContent::Text("hello from A ✓".into()));
    a.engine.feed_system(SystemEvent::ClipboardChanged);
    wait_for("text on B", || *b.desktop.clip.lock() == Some(ClipContent::Text("hello from A ✓".into()))).await;

    // B's clipboard watcher fires for the write it just made: must not echo back.
    let seq_a = a.desktop.clipboard_sequence();
    b.engine.feed_system(SystemEvent::ClipboardChanged);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.desktop.clipboard_sequence(), seq_a, "clipboard echoed back");

    // Images (large, multi-chunk) go the other way.
    let image: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    b.desktop.set(ClipContent::Image(image.clone()));
    b.engine.feed_system(SystemEvent::ClipboardChanged);
    wait_for("image on A", || *a.desktop.clip.lock() == Some(ClipContent::Image(image.clone()))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn copied_files_are_delivered_when_control_moves() {
    let (a, b) = paired_pair().await;
    let src = scratch("src");
    std::fs::write(src.join("notes.txt"), b"file body").unwrap();
    std::fs::create_dir_all(src.join("folder/sub")).unwrap();
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 7) as u8).collect();
    std::fs::write(src.join("folder/sub/big.bin"), &big).unwrap();

    a.desktop.set(ClipContent::Files(vec![src.join("notes.txt"), src.join("folder")]));
    a.engine.feed_system(SystemEvent::ClipboardChanged);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(b.desktop.clip.lock().is_none(), "files must not be sent before control moves");

    // Move to B: the files arrive and are offered on B's clipboard.
    a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 });
    wait_for("files on B's clipboard", || matches!(&*b.desktop.clip.lock(), Some(ClipContent::Files(f)) if f.len() == 2)).await;
    let Some(ClipContent::Files(files)) = b.desktop.clip.lock().clone() else { unreachable!() };
    let notes = files.iter().find(|p| p.ends_with("notes.txt")).unwrap();
    assert_eq!(std::fs::read(notes).unwrap(), b"file body");
    let folder = files.iter().find(|p| p.ends_with("folder")).unwrap();
    assert_eq!(std::fs::read(folder.join("sub").join("big.bin")).unwrap(), big);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dragging_files_across_the_edge_drops_them_on_the_other_pc() {
    let _ = tracing_subscriber::fmt().with_env_filter("mkl_core=debug").with_test_writer().try_init();
    let (a, b) = paired_pair().await;
    let src = scratch("drag");
    std::fs::write(src.join("photo.jpg"), b"jpeg bytes").unwrap();

    // Left button down, drag towards the right edge: the drop catcher appears.
    a.engine.feed_input(HookEvent::Button { button: MouseButton::Left, down: true });
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1905, y: 500 }), Disposition::Pass);
    assert!(a.desktop.catcher.lock().is_some(), "catcher should be shown near the edge");
    // The drag source reports its files to the catcher, the cursor reaches the edge.
    a.engine.feed_drag_files(vec![src.join("photo.jpg")]);
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    assert_eq!(a.engine.snapshot().active_target.as_deref(), Some(b.engine.device_id()));
    // Releasing on B drops the files there; A's drag is cancelled with Escape + release.
    assert_eq!(a.engine.feed_input(HookEvent::Button { button: MouseButton::Left, down: false }), Disposition::Swallow);

    let dest = b.engine.receive_dir().join("photo.jpg");
    wait_for("file saved on B", || std::fs::read(&dest).map(|d| d == b"jpeg bytes").unwrap_or(false)).await;
    wait_for("B reveals the file", || b.desktop.revealed.lock().contains(&dest)).await;
    wait_for("A cancelled its local drag", || {
        let ev = a.injector.events.lock();
        ev.contains(&InputEvent::Key { vk: 0x1B, scan: 0x01, extended: false, down: true })
            && ev.contains(&InputEvent::Button { button: MouseButton::Left, down: false })
    })
    .await;
    // A second drop of the same name doesn't overwrite the first.
    assert!(!b.engine.receive_dir().join("photo (1).jpg").exists());
}

/// Files dragged on the controlled PC (B) and carried back across the edge land on A.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dragging_files_from_the_controlled_pc_back_to_the_controller() {
    let (a, b) = paired_pair().await;
    let src = scratch("rdrag");
    std::fs::write(src.join("report.pdf"), b"pdf bytes").unwrap();

    a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }); // A -> B, cursor at B (2, 500)
    wait_for("B controlled", || b.engine.snapshot().controlled_by.is_some()).await;
    tokio::time::sleep(Duration::from_millis(300)).await; // switch cooldown

    // Press on a file in B's Explorer and start dragging it towards B's left edge (towards A).
    a.engine.feed_input(HookEvent::Button { button: MouseButton::Left, down: true });
    a.engine.feed_input(HookEvent::MouseMove { x: 961, y: 540 }); // B cursor (3, 500)
    wait_for("B shows its drop catcher", || b.desktop.catcher.lock().is_some()).await;
    b.engine.feed_drag_files(vec![src.join("report.pdf")]);

    // Keep going left: control returns to A, B's button is released, the drag drops on the catcher.
    a.engine.feed_input(HookEvent::MouseMove { x: 930, y: 540 });
    assert!(a.engine.snapshot().active_target.is_none(), "control should be back on A");
    wait_for("B's left button released", || b.injector.events.lock().contains(&InputEvent::Button { button: MouseButton::Left, down: false })).await;
    b.engine.feed_drag_dropped();

    let dest = a.engine.receive_dir().join("report.pdf");
    wait_for("file saved on A", || std::fs::read(&dest).map(|d| d == b"pdf bytes").unwrap_or(false)).await;
    wait_for("A reveals the file", || a.desktop.revealed.lock().contains(&dest)).await;
}

/// Ctrl+C on the controlled PC immediately followed by moving back: files still arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_copied_on_controlled_pc_reach_controller_without_change_event() {
    let (a, b) = paired_pair().await;
    let src = scratch("rcopy");
    std::fs::write(src.join("data.csv"), b"a,b\n1,2\n").unwrap();

    a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 });
    wait_for("B controlled", || b.engine.snapshot().controlled_by.is_some()).await;
    // Copy happens on B, but its clipboard notification hasn't been processed yet.
    b.desktop.set(ClipContent::Files(vec![src.join("data.csv")]));
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.engine.feed_input(HookEvent::MouseMove { x: 900, y: 540 }); // back across B's left edge
    assert!(a.engine.snapshot().active_target.is_none());

    wait_for("files on A's clipboard", || matches!(&*a.desktop.clip.lock(), Some(ClipContent::Files(f)) if f.len() == 1)).await;
    let Some(ClipContent::Files(files)) = a.desktop.clip.lock().clone() else { unreachable!() };
    assert_eq!(std::fs::read(&files[0]).unwrap(), b"a,b\n1,2\n");
}

/// Lock / unlock cycle on the controlled PC: control returns, entering is refused while it is
/// locked, and edge switching works again as soon as it unlocks (no reconnect, no restart).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn locked_pc_is_skipped_and_resumes_after_unlock() {
    let (a, b) = paired_pair().await;
    let bid = b.engine.device_id().to_string();
    let locked = |n: &Node| n.engine.snapshot().devices.iter().any(|d| d.id == bid && d.remote_locked);

    // A is controlling B when B locks: control returns to A immediately.
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    wait_for("B controlled", || b.engine.snapshot().controlled_by.is_some()).await;
    b.engine.feed_system(SystemEvent::SessionLocked);
    wait_for("A sees B locked", || locked(&a)).await;
    assert!(a.engine.snapshot().active_target.is_none(), "control must return to A when B locks");
    wait_for("B released control", || b.engine.snapshot().controlled_by.is_none()).await;

    // While B is locked, pushing against the edge stays on A.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Pass);
    assert!(a.engine.snapshot().active_target.is_none());
    assert!(online(&a, &bid), "the connection itself stays up while locked");

    // B unlocks: the same session resumes.
    b.engine.feed_system(SystemEvent::SessionUnlocked);
    wait_for("A sees B unlocked", || !locked(&a)).await;
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    assert_eq!(a.engine.snapshot().active_target.as_deref(), Some(bid.as_str()));
    wait_for("B controlled again", || b.engine.snapshot().controlled_by.is_some()).await;
}

/// The controlling PC locking also hands control back and resumes after unlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn controller_lock_returns_control_and_resumes() {
    let (a, b) = paired_pair().await;
    let bid = b.engine.device_id().to_string();
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    a.engine.feed_system(SystemEvent::SessionLocked);
    assert!(a.engine.snapshot().active_target.is_none());
    wait_for("B released", || b.engine.snapshot().controlled_by.is_none()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Pass, "locked PC must not switch");
    a.engine.feed_system(SystemEvent::SessionUnlocked);
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    assert_eq!(a.engine.snapshot().active_target.as_deref(), Some(bid.as_str()));
}

/// A high-rate mouse (1 kHz) is paced on the wire, but the final position always arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_mouse_moves_are_paced_without_losing_the_final_position() {
    let (a, b) = paired_pair().await;
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    let a = Arc::new(a);
    let feeder = a.clone();
    // Like the hook thread: one 1 px move every millisecond for 200 ms.
    tokio::task::spawn_blocking(move || {
        let start = std::time::Instant::now();
        for i in 0..200u64 {
            while start.elapsed() < Duration::from_millis(i) {
                std::hint::spin_loop();
            }
            feeder.engine.feed_input(HookEvent::MouseMove { x: 961, y: 540 });
        }
    })
    .await
    .unwrap();
    let last = InputEvent::MouseMove { x: 202, y: 500 };
    wait_for("final position on B", || b.injector.events.lock().last() == Some(&last)).await;
    let moves = b.injector.events.lock().iter().filter(|e| matches!(e, InputEvent::MouseMove { .. })).count();
    assert!(moves <= 80, "200 moves in 200 ms should be paced to ~50, B injected {moves}");
}

/// A click right after a held (paced) move is injected at the moved-to position.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn click_after_a_paced_move_lands_at_the_new_position() {
    let (a, b) = paired_pair().await;
    assert_eq!(a.engine.feed_input(HookEvent::MouseMove { x: 1919, y: 500 }), Disposition::Swallow);
    a.engine.feed_input(HookEvent::MouseMove { x: 961, y: 540 });
    a.engine.feed_input(HookEvent::MouseMove { x: 970, y: 540 });
    a.engine.feed_input(HookEvent::Button { button: MouseButton::Left, down: true });
    let click = InputEvent::Button { button: MouseButton::Left, down: true };
    wait_for("click on B", || b.injector.events.lock().contains(&click)).await;
    let got = b.injector.events.lock().clone();
    let i = got.iter().position(|e| *e == click).unwrap();
    assert_eq!(got[i - 1], InputEvent::MouseMove { x: 13, y: 500 });
}
