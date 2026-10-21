use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use mkl_discovery::{Discovery, DiscoveryEvent};
use mkl_input::{CaptureHandle, Disposition, HookEvent, InputInjector};
use mkl_protocol::{fingerprint, pair_uri, parse_pair_target, Keypair, LayoutSync, Message, Rect};
use mkl_store::Store;
use parking_lot::{Mutex, RwLock};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Notify};
use tracing::{info, warn};

use crate::layout::Layout;
use crate::net::{self, DialIntent};
use crate::router::{self, HookSink};
use crate::settings::Settings;
use crate::state::{PairDecision, PeerConn, Shared};
use crate::views::*;

pub struct EngineConfig {
    pub store: Arc<Store>,
    pub injector: Arc<dyn InputInjector>,
    /// Overrides the configured port (tests use 0 for an ephemeral port).
    pub listen_port: Option<u16>,
    pub enable_capture: bool,
    pub enable_discovery: bool,
    /// Clipboard and drag-and-drop integration.
    pub desktop: Arc<dyn mkl_input::Desktop>,
    /// Folder for files dragged here from another PC.
    pub receive_dir: std::path::PathBuf,
    /// Scratch folder for files received through the clipboard.
    pub temp_dir: std::path::PathBuf,
}

/// Public façade over the running engine. Cheap to share behind an `Arc` or app state.
pub struct Engine {
    shared: Arc<Shared>,
    capture: Mutex<Option<CaptureHandle>>,
    discovery: Mutex<Option<Discovery>>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Engine {
    /// Starts networking, discovery, reconnection and (optionally) the input hooks.
    /// Must be called from within a Tokio runtime.
    pub async fn start(cfg: EngineConfig) -> Result<(Engine, mpsc::UnboundedReceiver<EngineEvent>)> {
        let (private, public) = cfg.store.load_or_create_identity(|| {
            let kp = Keypair::generate();
            (kp.private, kp.public)
        })?;
        let keys = Keypair { private, public };
        let me_id = mkl_protocol::device_id(&keys.public);

        let settings: Settings = cfg.store.get_setting("settings")?.unwrap_or_default();
        let settings = settings.normalized();
        let port = cfg.listen_port.unwrap_or(settings.listen_port);
        let listener = match TcpListener::bind(("0.0.0.0", port)).await {
            Ok(l) => l,
            Err(e) => {
                warn!("port {port} unavailable ({e}); falling back to an ephemeral port");
                TcpListener::bind(("0.0.0.0", 0)).await.context("could not open a listening socket")?
            }
        };
        let listen_port = listener.local_addr()?.port();

        let mut layout = Layout {
            positions: cfg.store.load_layout()?.into_iter().map(|(id, x, y)| (id, (x, y))).collect(),
            revision: cfg.store.get_setting("layout_revision")?.unwrap_or(0),
        };
        layout.positions.entry(me_id.clone()).or_insert((0, 0));

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let screens = cfg.injector.screens();
        crate::transfer::clean_temp(&cfg.temp_dir);
        let discovery_enabled = settings.discovery_enabled;
        let device_name = settings.device_name.clone();

        let inject_state = crate::inject::InjectState::default();
        let shared = Arc::new(Shared {
            rt: tokio::runtime::Handle::current(),
            me_id: me_id.clone(),
            keys,
            store: cfg.store,
            injector: cfg.injector,
            settings: RwLock::new(settings),
            listen_port: AtomicU16::new(listen_port),
            peers: Default::default(),
            discovered: Default::default(),
            layout: RwLock::new(layout),
            local_screens: RwLock::new(screens),
            router: Default::default(),
            being_controlled: inject_state.active_flag(),
            last_controller: Default::default(),
            last_blocked_warning: AtomicU64::new(0),
            inject: Mutex::new(inject_state),
            events: events_tx,
            pending_pairs: Default::default(),
            dialing: Default::default(),
            paused: Default::default(),
            backoff: Default::default(),
            disconnected_at: Default::default(),
            reconnect_now: Notify::new(),
            locked: AtomicBool::new(false),
            capture_running: AtomicBool::new(false),
            discovery_running: AtomicBool::new(false),
            stats: Default::default(),
            next_id: AtomicU64::new(0),
            started: Instant::now(),
            desktop: cfg.desktop,
            receive_dir: cfg.receive_dir,
            temp_dir: cfg.temp_dir,
            clip: Default::default(),
            clip_refresh: Default::default(),
            incoming: Default::default(),
            refused: Default::default(),
            cancelled_out: Default::default(),
        });
        info!(device = %me_id, port = listen_port, "engine starting");

        shared.rt.spawn(net::run_listener(shared.clone(), listener));
        shared.rt.spawn(crate::reconnect::run(shared.clone()));

        let capture = if cfg.enable_capture {
            match mkl_input::start_capture(Arc::new(HookSink(shared.clone()))) {
                Ok(h) => {
                    shared.capture_running.store(true, Relaxed);
                    Some(h)
                }
                Err(e) => {
                    warn!("input capture unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };

        let discovery = if cfg.enable_discovery && discovery_enabled {
            let (dtx, mut drx) = mpsc::unbounded_channel();
            match Discovery::start(&me_id, &device_name, listen_port, move |ev| {
                let _ = dtx.send(ev);
            }) {
                Ok(d) => {
                    shared.discovery_running.store(true, Relaxed);
                    let s = shared.clone();
                    shared.rt.spawn(async move {
                        while let Some(ev) = drx.recv().await {
                            on_discovery(&s, ev);
                        }
                    });
                    Some(d)
                }
                Err(e) => {
                    warn!("LAN discovery unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };

        Ok((Engine { shared, capture: Mutex::new(capture), discovery: Mutex::new(discovery) }, events_rx))
    }

    pub fn device_id(&self) -> &str {
        &self.shared.me_id
    }

    pub fn listen_port(&self) -> u16 {
        self.shared.listen_port.load(Relaxed)
    }

    /// Feeds a hook event through the router exactly as the OS hook would (used by tests).
    pub fn feed_input(&self, ev: HookEvent) -> Disposition {
        router::handle_input(&self.shared, ev)
    }

    /// Feeds a system notification as the hook thread would (used by tests).
    pub fn feed_system(&self, ev: mkl_input::SystemEvent) {
        router::handle_system(&self.shared, ev);
    }

    /// Reports files dragged onto the edge drop catcher (used by tests).
    pub fn feed_drag_dropped(&self) {
        router::on_drag_dropped(&self.shared);
    }

    /// Reports files dragged onto the edge drop catcher (used by tests).
    pub fn feed_drag_files(&self, files: Vec<std::path::PathBuf>) {
        router::on_drag_files(&self.shared, files);
    }

    /// Folder where files dropped from other PCs are saved.
    pub fn receive_dir(&self) -> &std::path::Path {
        &self.shared.receive_dir
    }

    pub fn snapshot(&self) -> Snapshot {
        let s = &self.shared;
        let records = s.store.list_devices().unwrap_or_default();
        let peers = s.peers.read().clone();
        let discovered = s.discovered.read().clone();
        let paused = s.paused.lock().clone();
        let dialing = s.dialing.lock().clone();
        let active = router::active_target(s);
        let controlled_by = s.inject.lock().controlled_by.clone();

        let mut seen = HashSet::new();
        let mut devices = Vec::new();
        for r in records {
            seen.insert(r.device_id.clone());
            let live = peers.get(&r.device_id);
            let status = if r.blocked {
                DeviceStatus::Blocked
            } else if live.is_some() {
                DeviceStatus::Online
            } else if dialing.contains(&r.device_id) {
                DeviceStatus::Connecting
            } else if r.trusted {
                DeviceStatus::Offline
            } else if discovered.contains_key(&r.device_id) {
                DeviceStatus::Discovered
            } else {
                DeviceStatus::Offline
            };
            devices.push(DeviceView {
                id: r.device_id.clone(),
                name: live.map(|p| p.name.clone()).unwrap_or(r.name),
                status,
                trusted: r.trusted,
                blocked: r.blocked,
                paused: paused.contains(&r.device_id),
                addr: live.map(|p| p.addr.to_string()).or(r.last_addr),
                last_seen: r.last_seen,
                paired_at: r.paired_at,
                rtt_ms: live.and_then(|p| rtt_ms(p)),
                screens: live.map(|p| p.screens.read().clone()).unwrap_or(r.screens),
                remote_locked: live.is_some_and(|p| p.remote_locked.load(Relaxed)),
                fingerprint: Some(fingerprint(&r.public_key)),
                controlling: active.as_deref() == Some(r.device_id.as_str()),
                controlling_us: controlled_by.as_deref() == Some(r.device_id.as_str()),
            });
        }
        for (id, d) in discovered {
            if seen.contains(&id) {
                continue;
            }
            devices.push(DeviceView {
                id: id.clone(),
                name: d.name,
                status: DeviceStatus::Discovered,
                trusted: false,
                blocked: false,
                paused: false,
                addr: d.addrs.first().map(|a| a.to_string()),
                last_seen: None,
                paired_at: None,
                rtt_ms: None,
                screens: Vec::new(),
                remote_locked: false,
                fingerprint: None,
                controlling: false,
                controlling_us: false,
            });
        }

        Snapshot {
            me: self.me_view(),
            devices,
            active_target: active,
            controlled_by,
            locked: s.locked.load(Relaxed),
            capture_running: s.capture_running.load(Relaxed),
            discovery_running: s.discovery_running.load(Relaxed),
        }
    }

    fn me_view(&self) -> MeView {
        let s = &self.shared;
        let port = s.listen_port.load(Relaxed);
        let addresses = local_addresses();
        let host = addresses.first().cloned().unwrap_or_else(|| "127.0.0.1".into());
        MeView {
            id: s.me_id.clone(),
            name: s.settings.read().device_name.clone(),
            port,
            pair_uri: pair_uri(&host, port, &s.me_id),
            addresses,
            fingerprint: fingerprint(&s.keys.public),
            screens: s.local_screens.read().clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Starts pairing with a manually entered address or a `mkl://` link.
    pub fn pair_with(&self, target: &str) -> Result<()> {
        let t = parse_pair_target(target).ok_or_else(|| anyhow!("Enter an IP address (e.g. 192.168.1.20) or a pairing link"))?;
        if t.device_id.as_deref() == Some(self.shared.me_id.as_str()) {
            bail!("That pairing link belongs to this PC");
        }
        let shared = self.shared.clone();
        self.shared.rt.spawn(async move {
            let result = async {
                let addrs: Vec<SocketAddr> = match t.host.parse::<IpAddr>() {
                    Ok(ip) => vec![SocketAddr::new(ip, t.port)],
                    Err(_) => tokio::net::lookup_host((t.host.as_str(), t.port))
                        .await
                        .with_context(|| format!("could not resolve {}", t.host))?
                        .collect(),
                };
                let addr = *addrs.first().ok_or_else(|| anyhow!("no address for {}", t.host))?;
                net::dial(shared.clone(), addr, DialIntent { pairing: true, expected_id: t.device_id.clone() }).await
            }
            .await;
            if let Err(e) = result {
                shared.emit(EngineEvent::PairResult {
                    device_id: t.device_id,
                    name: t.host,
                    success: false,
                    message: format!("{e:#}"),
                });
            }
        });
        Ok(())
    }

    /// Starts pairing with a device found by LAN discovery.
    pub fn pair_device(&self, device_id: &str) -> Result<()> {
        let peer = self
            .shared
            .discovered
            .read()
            .get(device_id)
            .cloned()
            .ok_or_else(|| anyhow!("That device is no longer visible on the network"))?;
        let shared = self.shared.clone();
        let id = device_id.to_string();
        self.shared.rt.spawn(async move {
            let mut last_err = None;
            for addr in &peer.addrs {
                match net::dial(shared.clone(), *addr, DialIntent { pairing: true, expected_id: Some(id.clone()) }).await {
                    Ok(()) => return,
                    // Keep the first failure: it is for the preferred (IPv4) address.
                    Err(e) => last_err = last_err.or(Some(e)),
                }
            }
            shared.emit(EngineEvent::PairResult {
                device_id: Some(id),
                name: peer.name,
                success: false,
                message: last_err.map(|e| format!("{e:#}")).unwrap_or_else(|| "no reachable address".into()),
            });
        });
        Ok(())
    }

    pub fn respond_pairing(&self, request_id: u64, approve: bool, block: bool) -> Result<()> {
        let tx = self
            .shared
            .pending_pairs
            .lock()
            .remove(&request_id)
            .ok_or_else(|| anyhow!("This pairing request has expired"))?;
        let _ = tx.send(PairDecision { approve, block });
        Ok(())
    }

    /// Disconnects and stops auto-reconnecting until [`connect`](Self::connect) is called.
    pub fn disconnect(&self, device_id: &str) {
        self.shared.paused.lock().insert(device_id.to_string());
        if let Some(p) = self.shared.peer(device_id) {
            p.close();
        }
        self.shared.changed();
    }

    pub fn connect(&self, device_id: &str) {
        self.shared.paused.lock().remove(device_id);
        self.shared.backoff.lock().remove(device_id);
        self.shared.reconnect_now.notify_one();
        self.shared.changed();
    }

    /// Revokes trust and removes the device. It must pair again to reconnect.
    pub fn forget(&self, device_id: &str) -> Result<()> {
        self.shared.store.delete_device(device_id)?;
        self.shared.layout.write().positions.remove(device_id);
        if let Some(p) = self.shared.peer(device_id) {
            p.close();
        }
        self.shared.changed();
        Ok(())
    }

    pub fn set_blocked(&self, device_id: &str, blocked: bool) -> Result<()> {
        let s = &self.shared;
        if blocked {
            let rec = s.store.get_device(device_id)?;
            let name = rec
                .as_ref()
                .map(|r| r.name.clone())
                .or_else(|| s.discovered.read().get(device_id).map(|d| d.name.clone()))
                .unwrap_or_else(|| device_id.to_string());
            let key = rec.map(|r| r.public_key).unwrap_or_default();
            s.store.block_device(device_id, &name, &key)?;
            if let Some(p) = s.peer(device_id) {
                p.close();
            }
        } else {
            s.store.unblock_device(device_id)?;
        }
        s.changed();
        Ok(())
    }

    pub fn return_control(&self) {
        router::return_local(&self.shared);
    }

    pub fn settings(&self) -> Settings {
        self.shared.settings.read().clone()
    }

    pub fn update_settings(&self, new: Settings) -> Result<Settings> {
        let new = new.normalized();
        self.shared.store.set_setting("settings", &new)?;
        *self.shared.settings.write() = new.clone();
        self.shared.reconnect_now.notify_one();
        self.shared.changed();
        Ok(new)
    }

    pub fn layout_view(&self) -> LayoutView {
        let s = &self.shared;
        let screens = s.all_known_screens();
        let layout = s.layout.read().clone();
        let peers = s.peers.read();
        let mut devices = vec![LayoutDevice {
            id: s.me_id.clone(),
            name: s.settings.read().device_name.clone(),
            is_local: true,
            online: true,
            x: layout.positions.get(&s.me_id).map_or(0, |p| p.0),
            y: layout.positions.get(&s.me_id).map_or(0, |p| p.1),
            screens: s.local_screens.read().clone(),
        }];
        let mut placed = layout.clone();
        for rec in s.store.list_devices().unwrap_or_default().into_iter().filter(|r| r.trusted) {
            let scr = screens.get(&rec.device_id).cloned().unwrap_or_else(|| vec![Rect::new(0, 0, 1920, 1080)]);
            let pos = match placed.positions.get(&rec.device_id) {
                Some(p) => *p,
                None => {
                    let p = placed.default_position(&screens);
                    placed.positions.insert(rec.device_id.clone(), p);
                    p
                }
            };
            devices.push(LayoutDevice {
                id: rec.device_id.clone(),
                name: peers.get(&rec.device_id).map(|p| p.name.clone()).unwrap_or(rec.name),
                is_local: false,
                online: peers.contains_key(&rec.device_id),
                x: pos.0,
                y: pos.1,
                screens: scr,
            });
        }
        LayoutView { revision: layout.revision, devices }
    }

    pub fn save_layout(&self, positions: Vec<LayoutPos>) -> Result<()> {
        let s = &self.shared;
        let known: HashSet<String> = s
            .store
            .list_devices()?
            .into_iter()
            .filter(|d| d.trusted)
            .map(|d| d.device_id)
            .chain(std::iter::once(s.me_id.clone()))
            .collect();
        let mut map = HashMap::new();
        for p in positions {
            if known.contains(&p.id) && p.x.abs() < 1_000_000 && p.y.abs() < 1_000_000 {
                map.insert(p.id, (p.x, p.y));
            }
        }
        if !map.contains_key(&s.me_id) {
            bail!("layout must include this PC");
        }
        {
            let mut l = s.layout.write();
            l.positions = map;
            l.revision = now_ms().max(l.revision + 1);
        }
        persist_layout(s);
        s.broadcast(Message::Layout(s.layout.read().to_sync()));
        s.changed();
        Ok(())
    }

    pub fn diagnostics(&self) -> Diagnostics {
        let s = &self.shared;
        let st = &s.stats;
        let events = st.hook_events.load(Relaxed);
        let peers = s
            .peers
            .read()
            .values()
            .map(|p| {
                let rtt = rtt_ms(p);
                PeerDiag {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    addr: p.addr.to_string(),
                    rtt_ms: rtt,
                    quality: quality_for(rtt).to_string(),
                    connected_secs: p.connected_at.elapsed().as_secs(),
                    sent: p.stats.sent.load(Relaxed),
                    received: p.stats.received.load(Relaxed),
                    dropped: p.stats.dropped.load(Relaxed),
                    remote_locked: p.remote_locked.load(Relaxed),
                }
            })
            .collect();
        let reconnects = st.reconnects.load(Relaxed);
        Diagnostics {
            device_id: s.me_id.clone(),
            uptime_secs: s.started.elapsed().as_secs(),
            listen_port: s.listen_port.load(Relaxed),
            capture_running: s.capture_running.load(Relaxed),
            discovery_running: s.discovery_running.load(Relaxed),
            locked: s.locked.load(Relaxed),
            hook_events: events,
            hook_avg_us: if events == 0 { 0.0 } else { st.hook_total_ns.load(Relaxed) as f64 / events as f64 / 1000.0 },
            hook_max_us: st.hook_max_ns.load(Relaxed) as f64 / 1000.0,
            forwarded: st.forwarded.load(Relaxed),
            injected: st.injected.load(Relaxed),
            inject_errors: st.inject_errors.load(Relaxed),
            unauthorized: st.unauthorized.load(Relaxed),
            switches: st.switches.load(Relaxed),
            reconnects,
            last_reconnect_ms: (reconnects > 0).then(|| st.last_reconnect_ms.load(Relaxed)),
            keys_held_by_remote: s.inject.lock().pressed_count(),
            screens: s.local_screens.read().clone(),
            peers,
        }
    }

    /// Releases everything and closes sessions. Call before the process exits.
    pub fn shutdown(&self) {
        let s = &self.shared;
        router::return_local(s);
        {
            let mut inj = s.inject.lock();
            if let Some(c) = inj.controlled_by.clone() {
                inj.end(&c, &*s.injector);
            }
        }
        for p in s.peers.read().values() {
            p.close();
        }
        self.capture.lock().take();
        self.discovery.lock().take();
        s.capture_running.store(false, Relaxed);
        s.discovery_running.store(false, Relaxed);
        info!("engine stopped");
    }
}

fn rtt_ms(p: &PeerConn) -> Option<f64> {
    let us = p.rtt_us.load(Relaxed);
    (us > 0).then(|| us as f64 / 1000.0)
}

fn local_addresses() -> Vec<String> {
    let mut v: Vec<IpAddr> = local_ip_address::list_afinet_netifas()
        .unwrap_or_default()
        .into_iter()
        .map(|(_, ip)| ip)
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
            IpAddr::V6(_) => false,
        })
        .collect();
    // Prefer typical home/office LAN ranges over VPN / virtual adapters.
    v.sort_by_key(|ip| match ip {
        IpAddr::V4(v4) if v4.octets()[0] == 192 && v4.octets()[1] == 168 => 0,
        IpAddr::V4(v4) if v4.octets()[0] == 10 => 1,
        IpAddr::V4(v4) if v4.is_private() => 2,
        _ => 3,
    });
    v.dedup();
    v.into_iter().map(|ip| ip.to_string()).collect()
}

fn persist_layout(s: &Shared) {
    let l = s.layout.read().clone();
    let rows: Vec<_> = l.positions.iter().map(|(id, (x, y))| (id.clone(), *x, *y)).collect();
    if let Err(e) = s.store.save_layout(&rows).and_then(|_| s.store.set_setting("layout_revision", &l.revision)) {
        warn!("could not save layout: {e}");
    }
}

fn on_discovery(s: &Arc<Shared>, ev: DiscoveryEvent) {
    match ev {
        DiscoveryEvent::Found(p) => {
            let id = p.device_id.clone();
            let is_new = s.discovered.read().get(&id) != Some(&p);
            s.discovered.write().insert(id.clone(), p);
            if is_new {
                if !s.peers.read().contains_key(&id) {
                    s.backoff.lock().remove(&id);
                    s.reconnect_now.notify_one();
                }
                s.changed();
            }
        }
        DiscoveryEvent::Lost(id) => {
            if s.discovered.write().remove(&id).is_some() {
                s.changed();
            }
        }
    }
}

/// Called when a session is established: place the device in the layout if it has no
/// position yet, then exchange layouts so both sides converge on one arrangement.
///
/// On a first meeting both sides place each other; only the lower device id bumps the
/// revision, so its arrangement deterministically wins.
pub(crate) fn on_peer_connected(s: &Arc<Shared>, peer: &Arc<PeerConn>) {
    let placed = {
        let screens = s.all_known_screens();
        let mut l = s.layout.write();
        if l.positions.contains_key(&peer.id) {
            false
        } else {
            let pos = l.default_position(&screens);
            l.positions.insert(peer.id.clone(), pos);
            if s.me_id < peer.id {
                l.revision = now_ms().max(l.revision + 1);
            }
            true
        }
    };
    if placed {
        persist_layout(s);
    }
    peer.try_send(Message::Layout(s.layout.read().to_sync()));
    if s.locked.load(Relaxed) {
        peer.try_send(Message::Status { locked: true });
    }
    s.changed();
}

/// Layout convergence rules:
/// * adopt the peer's layout if it covers every device we have placed and is newer
///   (ties broken by the lower device id);
/// * if the peer knows strictly fewer devices than we do, keep ours, bump its revision and
///   send it back so the peer adopts it;
/// * otherwise (disjoint knowledge) keep ours.
pub(crate) fn apply_remote_layout(s: &Arc<Shared>, from: &str, sync: LayoutSync) {
    let incoming: HashMap<String, (i32, i32)> = sync.positions.into_iter().map(|(id, x, y)| (id, (x, y))).collect();
    if !incoming.contains_key(&s.me_id) || !incoming.contains_key(from) {
        return;
    }
    enum Outcome {
        Adopted,
        Reply,
        Ignore,
    }
    let outcome = {
        let mut l = s.layout.write();
        let covers_ours = l.positions.keys().all(|k| incoming.contains_key(k));
        let covered_by_ours = incoming.keys().all(|k| l.positions.contains_key(k));
        let newer = sync.revision > l.revision || (sync.revision == l.revision && from < s.me_id.as_str());
        if covers_ours && newer {
            l.positions = incoming;
            l.revision = sync.revision;
            Outcome::Adopted
        } else if covered_by_ours && !covers_ours {
            l.revision = l.revision.max(sync.revision) + 1;
            Outcome::Reply
        } else {
            Outcome::Ignore
        }
    };
    match outcome {
        Outcome::Adopted => {
            persist_layout(s);
            s.changed();
        }
        Outcome::Reply => {
            persist_layout(s);
            if let Some(p) = s.peer(from) {
                p.try_send(Message::Layout(s.layout.read().to_sync()));
            }
        }
        Outcome::Ignore => {}
    }
}
