//! Connection handling: Noise handshake → hello → mutual authorization (pairing) → session.
//!
//! A session only starts when *both* sides send `Accept`. A side accepts when the remote key
//! is already trusted, when its own user initiated the pairing, or when its user approved an
//! incoming request after comparing the six-digit code.

use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use mkl_protocol::{
    device_id, handshake, sanitize_screens, verification_code, Established, Message, SecureReader, SecureWriter,
    PROTOCOL_VERSION, REJECT_NOT_TRUSTED,
};
use mkl_store::{now_unix, DeviceRecord};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::state::{PairDecision, PeerConn, Shared};
use crate::views::EngineEvent;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(6);
const PAIRING_TIMEOUT: Duration = Duration::from_secs(120);
const HEARTBEAT: Duration = Duration::from_millis(1000);
const PEER_TIMEOUT: Duration = Duration::from_secs(5);
const OUTBOUND_QUEUE: usize = 512;
const WRITE_BATCH: usize = 64;
const BULK_QUEUE: usize = 8;
/// Minimum spacing of mouse moves on the wire (250 Hz). High-rate mice report up to every
/// 1 ms; sending each as its own packet floods Wi-Fi and delays everything behind it.
const MOVE_INTERVAL: Duration = Duration::from_millis(4);

fn is_move(m: &Message) -> bool {
    matches!(m, Message::Input(mkl_protocol::InputEvent::MouseMove { .. }))
}

/// Encrypts `batch` into one write. A move immediately followed by another move is skipped
/// (positions are absolute). Returns false if the connection failed.
async fn write_batch(
    writer: &mut SecureWriter,
    peer: &PeerConn,
    batch: &[Message],
    last_move: &mut tokio::time::Instant,
) -> bool {
    let mut n = 0u64;
    for (i, m) in batch.iter().enumerate() {
        if is_move(m) && batch.get(i + 1).is_some_and(is_move) {
            continue;
        }
        if writer.push(m).is_err() {
            return false;
        }
        if is_move(m) {
            *last_move = tokio::time::Instant::now();
        }
        n += 1;
    }
    if n == 0 {
        return true;
    }
    peer.stats.sent.fetch_add(n, Relaxed);
    writer.flush().await.is_ok()
}

#[derive(Debug, Clone, Default)]
pub struct DialIntent {
    /// The local user explicitly asked to pair with this address.
    pub pairing: bool,
    /// Abort unless the authenticated remote key hashes to this id.
    pub expected_id: Option<String>,
}

pub async fn run_listener(shared: Arc<Shared>, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let s = shared.clone();
                shared.rt.spawn(async move {
                    if let Err(e) = handle_connection(s, stream, addr, false, DialIntent::default()).await {
                        debug!(%addr, "inbound connection ended: {e:#}");
                    }
                });
            }
            Err(e) => {
                warn!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

/// Connects to `addr`. Returns `Err` if no session could be established; `Ok` once an
/// established session has ended.
pub async fn dial(shared: Arc<Shared>, addr: SocketAddr, intent: DialIntent) -> Result<()> {
    let stream = match timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
        Err(_) => bail!(
            "no answer from {addr}. Check that both PCs are on the same network and that no firewall \
             or antivirus is blocking MouseKey LAN (TCP {})",
            addr.port()
        ),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => bail!(
            "MouseKey LAN is not running on {} (connection refused). Start it on that PC; both PCs \
             need the same version",
            addr.ip()
        ),
        Ok(Err(e)) => return Err(e).with_context(|| format!("could not connect to {addr}")),
        Ok(Ok(s)) => s,
    };
    handle_connection(shared, stream, addr, true, intent).await
}

struct PendingGuard {
    shared: Arc<Shared>,
    request_id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.shared.pending_pairs.lock().remove(&self.request_id);
        self.shared.emit(EngineEvent::PairRequestClosed { request_id: self.request_id });
    }
}

fn clean_name(name: &str, fallback: &str) -> String {
    let n: String = name.chars().filter(|c| !c.is_control()).take(48).collect();
    if n.trim().is_empty() {
        fallback.to_string()
    } else {
        n.trim().to_string()
    }
}

async fn handle_connection(
    shared: Arc<Shared>,
    stream: TcpStream,
    addr: SocketAddr,
    initiator: bool,
    intent: DialIntent,
) -> Result<()> {
    let _ = stream.set_nodelay(true);
    let _priority = crate::qos::prioritize(&stream);
    let Established { remote_static, handshake_hash, mut reader, mut writer } =
        timeout(HANDSHAKE_TIMEOUT, handshake(stream, &shared.keys, initiator))
            .await
            .context("secure handshake timed out")?
            .context("secure handshake failed")?;

    let remote_id = device_id(&remote_static);
    if remote_id == shared.me_id {
        bail!("refusing to connect to this device itself");
    }
    if let Some(expected) = &intent.expected_id {
        if *expected != remote_id {
            bail!("device identity mismatch: expected {expected}, got {remote_id}");
        }
    }
    let record = shared.store.get_device(&remote_id)?;
    if record.as_ref().is_some_and(|r| r.blocked) {
        bail!("connection from blocked device {remote_id} refused");
    }

    writer.send(&Message::Hello(shared.local_hello(intent.pairing))).await?;
    let hello = match timeout(HANDSHAKE_TIMEOUT, reader.recv()).await.context("timed out waiting for hello")?? {
        Message::Hello(h) => h,
        other => bail!("unexpected message before hello: {other:?}"),
    };
    if hello.device_id != remote_id {
        bail!("hello device id does not match the authenticated key");
    }
    let name = clean_name(&hello.name, &remote_id);
    if hello.protocol != PROTOCOL_VERSION {
        let _ = writer.send(&Message::Reject { reason: "incompatible app version".into() }).await;
        bail!("{name} uses protocol {} (expected {PROTOCOL_VERSION})", hello.protocol);
    }

    let trusted = record.as_ref().is_some_and(|r| r.trusted && r.public_key == remote_static);
    let code = verification_code(&handshake_hash);
    let peer_addr = if initiator { addr } else { SocketAddr::new(addr.ip(), hello.listen_port) };

    // --- local authorization -------------------------------------------------------------
    let mut remote_accepted = false;
    if trusted || (initiator && intent.pairing) {
        if !trusted {
            shared.emit(EngineEvent::PairPending { device_id: remote_id.clone(), name: name.clone(), code: code.clone() });
        }
        writer.send(&Message::Accept).await?;
    } else if !initiator && hello.pairing && shared.settings.read().accept_pair_requests {
        let (tx, mut rx) = oneshot::channel::<PairDecision>();
        let request_id = shared.next_id();
        shared.pending_pairs.lock().insert(request_id, tx);
        let _guard = PendingGuard { shared: shared.clone(), request_id };
        info!(device = %remote_id, %name, %peer_addr, "incoming pairing request");
        shared.emit(EngineEvent::PairRequest {
            request_id,
            device_id: remote_id.clone(),
            name: name.clone(),
            addr: peer_addr.ip().to_string(),
            code: code.clone(),
        });
        let deadline = tokio::time::sleep(PAIRING_TIMEOUT);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                decision = &mut rx => match decision {
                    Ok(PairDecision { approve: true, .. }) => {
                        writer.send(&Message::Accept).await?;
                        break;
                    }
                    Ok(PairDecision { approve: false, block }) => {
                        let _ = writer.send(&Message::Reject { reason: "pairing request declined".into() }).await;
                        if block {
                            shared.store.block_device(&remote_id, &name, &remote_static)?;
                            shared.changed();
                        }
                        bail!("pairing with {name} declined");
                    }
                    Err(_) => bail!("pairing request abandoned"),
                },
                msg = reader.recv() => match msg? {
                    Message::Accept => remote_accepted = true,
                    Message::Reject { reason } => bail!("{name} cancelled pairing: {reason}"),
                    _ => {}
                },
                _ = &mut deadline => {
                    let _ = writer.send(&Message::Reject { reason: "pairing request timed out".into() }).await;
                    bail!("pairing request from {name} timed out");
                }
            }
        }
    } else {
        let _ = writer.send(&Message::Reject { reason: REJECT_NOT_TRUSTED.into() }).await;
        bail!("rejected connection from unpaired device {name} ({remote_id})");
    }

    // --- remote authorization ------------------------------------------------------------
    if !remote_accepted {
        let wait = if trusted { HANDSHAKE_TIMEOUT } else { PAIRING_TIMEOUT };
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let msg = tokio::time::timeout_at(deadline, reader.recv())
                .await
                .with_context(|| format!("timed out waiting for {name} to approve"))??;
            match msg {
                Message::Accept => break,
                Message::Reject { reason } => {
                    if reason == REJECT_NOT_TRUSTED && trusted && !intent.pairing {
                        // The other side forgot or revoked us: drop our trust too.
                        shared.store.set_trusted(&remote_id, false)?;
                        shared.emit(EngineEvent::Notice {
                            level: "warn".into(),
                            message: format!("{name} no longer trusts this PC. Pair again to reconnect."),
                        });
                        shared.changed();
                    }
                    bail!("{name} rejected the connection ({reason})");
                }
                _ => {}
            }
        }
    }

    // --- established -----------------------------------------------------------------------
    let screens = sanitize_screens(hello.screens);
    shared.store.save_device(&DeviceRecord {
        device_id: remote_id.clone(),
        name: name.clone(),
        public_key: remote_static,
        trusted: true,
        blocked: false,
        last_addr: Some(peer_addr.to_string()),
        last_seen: Some(now_unix()),
        paired_at: Some(now_unix()),
        screens: screens.clone(),
    })?;
    if !trusted {
        info!(device = %remote_id, %name, "paired");
        shared.emit(EngineEvent::PairResult {
            device_id: Some(remote_id.clone()),
            name: name.clone(),
            success: true,
            message: format!("Paired with {name}"),
        });
    }
    shared.paused.lock().remove(&remote_id);
    run_session(shared, reader, writer, remote_id, name, peer_addr, initiator, screens).await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    shared: Arc<Shared>,
    mut reader: SecureReader,
    mut writer: SecureWriter,
    id: String,
    name: String,
    addr: SocketAddr,
    initiator: bool,
    screens: Vec<mkl_protocol::Rect>,
) {
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE);
    let (bulk_tx, mut bulk_rx) = mpsc::channel::<Message>(BULK_QUEUE);
    let (close_tx, close_rx) = watch::channel(false);
    let peer = Arc::new(PeerConn {
        id: id.clone(),
        name: name.clone(),
        conn_id: shared.next_id(),
        addr,
        dialer: if initiator { shared.me_id.clone() } else { id.clone() },
        tx,
        bulk_tx,
        close_tx,
        screens: parking_lot::RwLock::new(screens),
        rtt_us: Default::default(),
        remote_locked: Default::default(),
        connected_at: Instant::now(),
        stats: Default::default(),
    });
    if !shared.register_peer(peer.clone()) {
        debug!(device = %id, "duplicate connection dropped");
        let _ = writer.send(&Message::Bye).await;
        writer.close().await;
        return;
    }
    info!(device = %id, %name, %addr, "connected");

    crate::engine::on_peer_connected(&shared, &peer);

    // Writer: drains everything queued into one encrypted write. Consecutive mouse moves are
    // coalesced into the newest one (positions are absolute), so a slow link never builds a
    // backlog of stale positions; keys, clicks and wheel are always kept and in order.
    // Moves are also paced to MOVE_INTERVAL: one arriving sooner is held (and replaced by any
    // newer move) until the interval ends, or sent ahead of the next non-move message.
    let mut wclose = close_rx.clone();
    let wpeer = peer.clone();
    let writer_task = shared.rt.spawn(async move {
        let mut held: Option<Message> = None;
        let mut last_move = tokio::time::Instant::now();
        loop {
            tokio::select! {
                biased;
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    let mut batch = Vec::with_capacity(16);
                    batch.extend(held.take());
                    batch.push(msg);
                    while batch.len() < WRITE_BATCH * 4 {
                        match rx.try_recv() {
                            Ok(m) => batch.push(m),
                            Err(_) => break,
                        }
                    }
                    if batch.last().is_some_and(is_move) && last_move.elapsed() < MOVE_INTERVAL {
                        held = batch.pop();
                        // Moves right before the held one are superseded by it.
                        while batch.last().is_some_and(is_move) {
                            batch.pop();
                        }
                    }
                    if !write_batch(&mut writer, &wpeer, &batch, &mut last_move).await { break; }
                }
                _ = tokio::time::sleep_until(last_move + MOVE_INTERVAL), if held.is_some() => {
                    let batch: Vec<Message> = held.take().into_iter().collect();
                    if !write_batch(&mut writer, &wpeer, &batch, &mut last_move).await { break; }
                }
                chunk = bulk_rx.recv() => {
                    // Bulk data (files, clipboard) only goes out when no input is waiting.
                    let Some(chunk) = chunk else { continue };
                    if writer.push(&chunk).is_err() || writer.flush().await.is_err() { break; }
                    wpeer.stats.sent.fetch_add(1, Relaxed);
                }
                _ = wclose.changed() => {
                    // Drain anything queued before the close (e.g. key releases), then say bye.
                    if let Some(m) = held.take() { let _ = writer.push(&m); }
                    while let Ok(m) = rx.try_recv() { let _ = writer.push(&m); }
                    let _ = writer.push(&Message::Bye);
                    let _ = timeout(Duration::from_millis(500), writer.flush()).await;
                    break;
                }
            }
        }
        writer.close().await;
    });

    // Heartbeat: also measures round-trip time.
    let mut pclose = close_rx.clone();
    let ppeer = peer.clone();
    let epoch = shared.started;
    let ping_task = shared.rt.spawn(async move {
        let mut iv = tokio::time::interval(HEARTBEAT);
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = iv.tick() => { ppeer.try_send(Message::Ping { nonce: epoch.elapsed().as_micros() as u64 }); }
                _ = pclose.changed() => break,
            }
        }
    });

    let mut rclose = close_rx;
    let mut pending_move: Option<Message> = None;
    let reason = loop {
        tokio::select! {
            msg = reader.recv() => match msg {
                Ok(Message::Bye) => break "closed by peer".to_string(),
                Ok(m) => {
                    peer.stats.received.fetch_add(1, Relaxed);
                    // A move followed by more already-received data may be stale: hold it and
                    // inject only the newest of a burst of consecutive moves.
                    if is_move(&m) {
                        if reader.has_buffered_frame() {
                            pending_move = Some(m);
                            continue;
                        }
                        pending_move = None;
                    } else if let Some(p) = pending_move.take() {
                        dispatch(&shared, &peer, p);
                    }
                    dispatch(&shared, &peer, m);
                }
                Err(e) => break e.to_string(),
            },
            _ = rclose.changed() => break "closed locally".to_string(),
            _ = tokio::time::sleep(PEER_TIMEOUT) => break "heartbeat timeout".to_string(),
        }
    };

    shared.unregister_peer(&peer);
    peer.close();
    let _ = writer_task.await;
    ping_task.abort();
    info!(device = %id, %name, "disconnected: {reason}");
}

fn dispatch(shared: &Arc<Shared>, peer: &Arc<PeerConn>, msg: Message) {
    match msg {
        Message::Ping { nonce } => {
            peer.try_send(Message::Pong { nonce });
        }
        Message::Pong { nonce } => {
            let now = shared.started.elapsed().as_micros() as u64;
            peer.rtt_us.store(now.saturating_sub(nonce).max(1), Relaxed);
        }
        Message::ControlEnter { x, y } => {
            let screens = shared.local_screens.read().clone();
            let mut inj = shared.inject.lock();
            inj.begin(&peer.id, &*shared.injector);
            *shared.last_controller.lock() = Some(peer.id.clone());
            let _ = inj.apply(&peer.id, mkl_protocol::InputEvent::MouseMove { x, y }, &*shared.injector, &screens);
            drop(inj);
            info!(device = %peer.id, "remote device took control of this PC");
            shared.changed();
        }
        Message::ControlLeave => {
            shared.inject.lock().end(&peer.id, &*shared.injector);
            crate::transfer::on_control_left(shared, &peer.id);
            crate::router::hide_controller_catcher_later(shared);
            shared.changed();
        }
        Message::ControlRevoked => {
            crate::router::on_remote_unavailable(shared, &peer.id);
        }
        Message::Input(ev) => {
            let screens = shared.local_screens.read().clone();
            let result = shared.inject.lock().apply(&peer.id, ev, &*shared.injector, &screens);
            match result {
                Ok(true) => {
                    shared.stats.injected.fetch_add(1, Relaxed);
                    if let mkl_protocol::InputEvent::Key { down: true, .. } = ev {
                        warn_if_typing_blocked(shared, peer);
                    }
                    if matches!(ev, mkl_protocol::InputEvent::MouseMove { .. }) {
                        let left = shared.inject.lock().is_down(mkl_protocol::MouseButton::Left);
                        let (cx, cy) = shared.injector.cursor_pos();
                        crate::router::on_injected_move(shared, &peer.id, cx, cy, left);
                    }
                }
                Ok(false) => {
                    shared.stats.unauthorized.fetch_add(1, Relaxed);
                }
                Err(e) => {
                    let n = shared.stats.inject_errors.fetch_add(1, Relaxed);
                    if n % 200 == 0 {
                        warn!("input injection failed: {e}");
                    }
                }
            }
        }
        Message::Screens(s) => {
            let s = sanitize_screens(s);
            let _ = shared.store.update_screens(&peer.id, &s);
            *peer.screens.write() = s;
            shared.changed();
        }
        Message::Layout(sync) => crate::engine::apply_remote_layout(shared, &peer.id, sync),
        m @ (Message::TransferStart { .. }
        | Message::TransferFile { .. }
        | Message::TransferChunk { .. }
        | Message::TransferEnd { .. }
        | Message::TransferCancel { .. }) => crate::transfer::on_message(shared, peer, m),
        Message::Status { locked } => {
            peer.remote_locked.store(locked, Relaxed);
            if locked {
                // Input cannot reach another machine's lock screen; take control back.
                crate::router::on_remote_unavailable(shared, &peer.id);
            }
            shared.changed();
        }
        Message::Notice { text } => {
            let text: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
            shared.emit(EngineEvent::Notice { level: "blocked".into(), message: format!("{}: {text}", peer.name) });
        }
        Message::Hello(_) | Message::Accept | Message::Reject { .. } | Message::Bye => {}
    }
}

/// Windows silently drops input sent to windows of elevated (administrator) processes unless
/// the sender is elevated too. Detect that case and tell both users, at most every 30 s.
fn warn_if_typing_blocked(shared: &Arc<Shared>, peer: &Arc<PeerConn>) {
    const EVERY_MS: u64 = 30_000;
    let now = shared.started.elapsed().as_millis() as u64;
    let last = shared.last_blocked_warning.load(Relaxed);
    if last != 0 && now.saturating_sub(last) < EVERY_MS {
        return;
    }
    if shared.desktop.is_elevated() || !shared.desktop.foreground_is_elevated() {
        return;
    }
    shared.last_blocked_warning.store(now.max(1), Relaxed);
    let text = "the active window there runs as administrator, so Windows blocks remote typing into it. \
                Turn on \"Run as administrator\" in MouseKey LAN Settings on that PC, or use its own keyboard for that window."
        .to_string();
    warn!("typing into an elevated window is blocked by Windows (UIPI)");
    peer.try_send(Message::Notice { text });
}
