use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mkl_discovery::DiscoveredPeer;
use mkl_input::{Desktop, InputInjector};
use mkl_protocol::{Hello, Keypair, Message, Rect, PROTOCOL_VERSION};
use mkl_store::Store;
use parking_lot::{Mutex, RwLock};
use tokio::sync::{mpsc, oneshot, watch, Notify};

use crate::inject::InjectState;
use crate::layout::Layout;
use crate::router::RouterState;
use crate::settings::Settings;
use crate::views::EngineEvent;

/// A user's answer to an incoming pairing request.
#[derive(Debug, Clone, Copy)]
pub struct PairDecision {
    pub approve: bool,
    pub block: bool,
}

/// Queue slots mouse moves may not use, so a burst of moves on a stalled link can never push
/// out keys, clicks or their releases.
const INPUT_RESERVE: usize = 64;

#[derive(Default)]
pub struct PeerStats {
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub dropped: AtomicU64,
}

/// One live, authenticated session with a paired device.
pub struct PeerConn {
    pub id: String,
    pub name: String,
    pub conn_id: u64,
    pub addr: SocketAddr,
    /// Device id of whoever opened the TCP connection (used to break duplicate-connection ties).
    pub dialer: String,
    pub tx: mpsc::Sender<Message>,
    /// Low-priority queue for file / clipboard transfers (awaited, never dropped).
    pub bulk_tx: mpsc::Sender<Message>,
    pub close_tx: watch::Sender<bool>,
    pub screens: RwLock<Vec<Rect>>,
    pub rtt_us: AtomicU64,
    pub remote_locked: AtomicBool,
    pub connected_at: Instant,
    pub stats: PeerStats,
}

impl PeerConn {
    /// Queues a message without blocking. Mouse moves are absolute, so dropping one under
    /// back-pressure is harmless; the next one supersedes it.
    pub fn try_send(&self, msg: Message) -> bool {
        if crate::net::is_move(&msg) && self.tx.capacity() <= INPUT_RESERVE {
            self.stats.dropped.fetch_add(1, Relaxed);
            return false;
        }
        match self.tx.try_send(msg) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.stats.dropped.fetch_add(1, Relaxed);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    pub fn close(&self) {
        let _ = self.close_tx.send(true);
    }
}

#[derive(Default)]
pub struct Stats {
    pub hook_events: AtomicU64,
    pub hook_total_ns: AtomicU64,
    pub hook_max_ns: AtomicU64,
    pub forwarded: AtomicU64,
    pub injected: AtomicU64,
    pub inject_errors: AtomicU64,
    pub unauthorized: AtomicU64,
    pub reconnects: AtomicU64,
    pub last_reconnect_ms: AtomicU64,
    pub switches: AtomicU64,
    pub transfers_received: AtomicU64,
}

pub struct Backoff {
    pub delay: Duration,
    pub base: Duration,
    pub next: Instant,
}

impl Backoff {
    pub const MAX: Duration = Duration::from_secs(30);

    pub fn new(base: Duration) -> Self {
        Self { delay: base, base, next: Instant::now() + base }
    }
    pub fn fail(&mut self) {
        self.next = Instant::now() + self.delay;
        self.delay = (self.delay * 2).min(Self::MAX);
    }
    pub fn reset(&mut self) {
        self.delay = self.base;
        self.next = Instant::now();
    }
}

pub struct Shared {
    pub rt: tokio::runtime::Handle,
    pub me_id: String,
    pub keys: Keypair,
    pub store: Arc<Store>,
    pub injector: Arc<dyn InputInjector>,
    pub settings: RwLock<Settings>,
    pub listen_port: AtomicU16,
    pub peers: RwLock<HashMap<String, Arc<PeerConn>>>,
    pub discovered: RwLock<HashMap<String, DiscoveredPeer>>,
    pub layout: RwLock<Layout>,
    pub local_screens: RwLock<Vec<Rect>>,
    pub router: Mutex<RouterState>,
    pub inject: Mutex<InjectState>,
    /// True while another device controls this PC (read by the hook without locking).
    pub being_controlled: Arc<std::sync::atomic::AtomicBool>,
    /// The most recent device that controlled this PC (for drags it carried back).
    pub last_controller: Mutex<Option<String>>,
    /// When the "typing blocked by an admin window" warning was last sent (ms since start).
    pub last_blocked_warning: AtomicU64,
    pub events: mpsc::UnboundedSender<EngineEvent>,
    pub pending_pairs: Mutex<HashMap<u64, oneshot::Sender<PairDecision>>>,
    pub dialing: Mutex<HashSet<String>>,
    pub paused: Mutex<HashSet<String>>,
    pub backoff: Mutex<HashMap<String, Backoff>>,
    pub disconnected_at: Mutex<HashMap<String, Instant>>,
    pub reconnect_now: Notify,
    pub locked: AtomicBool,
    /// What peers were last told with `Status`: this PC can't take remote input.
    pub announced_blocked: AtomicBool,
    pub capture_running: AtomicBool,
    pub discovery_running: AtomicBool,
    pub stats: Stats,
    pub next_id: AtomicU64,
    pub started: Instant,
    pub desktop: Arc<dyn Desktop>,
    /// Where dropped files are saved (Downloads\MouseKey LAN).
    pub receive_dir: std::path::PathBuf,
    /// Scratch space for files received through the clipboard.
    pub temp_dir: std::path::PathBuf,
    pub clip: Mutex<crate::transfer::ClipState>,
    /// Serializes clipboard reads.
    pub clip_refresh: tokio::sync::Mutex<()>,
    pub incoming: Mutex<HashMap<(String, u64), crate::transfer::Incoming>>,
    pub refused: Mutex<HashSet<(String, u64)>>,
    pub cancelled_out: Mutex<HashSet<u64>>,
}

impl Shared {
    pub fn emit(&self, ev: EngineEvent) {
        let _ = self.events.send(ev);
    }

    pub fn changed(&self) {
        self.emit(EngineEvent::Changed);
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Relaxed) + 1
    }

    pub fn peer(&self, id: &str) -> Option<Arc<PeerConn>> {
        self.peers.read().get(id).cloned()
    }

    pub fn broadcast(&self, msg: Message) {
        for p in self.peers.read().values() {
            p.try_send(msg.clone());
        }
    }

    /// This PC can't take remote input: it is locked and the lock-screen service can't reach
    /// the Windows password screen.
    pub fn input_blocked(&self) -> bool {
        self.locked.load(Relaxed) && !self.injector.secure_input()
    }

    /// Tells peers when [`input_blocked`](Self::input_blocked) changes.
    pub fn refresh_lock_status(&self) {
        let blocked = self.input_blocked();
        if self.announced_blocked.swap(blocked, Relaxed) != blocked {
            self.broadcast(Message::Status { locked: blocked });
        }
    }

    /// Monitors of this device and of every connected peer, keyed by device id.
    pub fn screens_map(&self) -> HashMap<String, Vec<Rect>> {
        let mut map = HashMap::new();
        map.insert(self.me_id.clone(), self.local_screens.read().clone());
        for (id, p) in self.peers.read().iter() {
            let s = p.screens.read();
            if !s.is_empty() {
                map.insert(id.clone(), s.clone());
            }
        }
        map
    }

    /// Monitors of every known device, including offline ones (from the database).
    pub fn all_known_screens(&self) -> HashMap<String, Vec<Rect>> {
        let mut map: HashMap<String, Vec<Rect>> = self
            .store
            .list_devices()
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.trusted && !d.screens.is_empty())
            .map(|d| (d.device_id, d.screens))
            .collect();
        map.extend(self.screens_map());
        map
    }

    pub fn local_hello(&self, pairing: bool) -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            device_id: self.me_id.clone(),
            name: self.settings.read().device_name.clone(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            listen_port: self.listen_port.load(Relaxed),
            screens: self.local_screens.read().clone(),
            pairing,
        }
    }

    pub fn record_hook_time(&self, elapsed: Duration) {
        let ns = elapsed.as_nanos() as u64;
        self.stats.hook_events.fetch_add(1, Relaxed);
        self.stats.hook_total_ns.fetch_add(ns, Relaxed);
        self.stats.hook_max_ns.fetch_max(ns, Relaxed);
    }

    /// Adds a newly established session, resolving duplicate connections deterministically:
    /// a reconnect from the same dialer replaces the stale session; otherwise the connection
    /// dialed by the lower device id wins.
    pub fn register_peer(&self, peer: Arc<PeerConn>) -> bool {
        let replaced = {
            let mut peers = self.peers.write();
            let replaced = match peers.get(&peer.id) {
                Some(existing) => {
                    let preferred = std::cmp::min(self.me_id.as_str(), peer.id.as_str());
                    if existing.dialer != peer.dialer && peer.dialer != preferred {
                        return false;
                    }
                    existing.close();
                    Some(existing.conn_id)
                }
                None => None,
            };
            peers.insert(peer.id.clone(), peer.clone());
            replaced
        };
        if let Some(old) = replaced {
            crate::router::on_peer_gone(self, old);
        }
        if let Some(t) = self.disconnected_at.lock().remove(&peer.id) {
            self.stats.reconnects.fetch_add(1, Relaxed);
            self.stats.last_reconnect_ms.store(t.elapsed().as_millis() as u64, Relaxed);
        }
        self.backoff.lock().remove(&peer.id);
        true
    }

    pub fn unregister_peer(&self, peer: &PeerConn) {
        let removed = {
            let mut peers = self.peers.write();
            if peers.get(&peer.id).is_some_and(|p| p.conn_id == peer.conn_id) {
                peers.remove(&peer.id);
                true
            } else {
                false
            }
        };
        crate::router::on_peer_gone(self, peer.conn_id);
        if removed {
            self.inject.lock().end(&peer.id, &*self.injector);
            self.disconnected_at.lock().insert(peer.id.clone(), Instant::now());
            let _ = self.store.touch_device(&peer.id, None);
            self.reconnect_now.notify_one();
            self.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mkl_protocol::{InputEvent, MouseButton};

    #[test]
    fn mouse_moves_cannot_fill_the_slots_reserved_for_clicks_and_keys() {
        let (tx, _rx) = mpsc::channel(512);
        let (bulk_tx, _bulk_rx) = mpsc::channel(1);
        let (close_tx, _close_rx) = watch::channel(false);
        let peer = PeerConn {
            id: "p".into(),
            name: "p".into(),
            conn_id: 1,
            addr: "127.0.0.1:1".parse().unwrap(),
            dialer: "p".into(),
            tx,
            bulk_tx,
            close_tx,
            screens: RwLock::new(Vec::new()),
            rtt_us: Default::default(),
            remote_locked: Default::default(),
            connected_at: Instant::now(),
            stats: Default::default(),
        };
        let moves = (0..1000).filter(|i| peer.try_send(Message::Input(InputEvent::MouseMove { x: *i, y: 0 }))).count();
        assert_eq!(moves, 512 - INPUT_RESERVE);
        assert_eq!(peer.stats.dropped.load(Relaxed), (1000 - moves) as u64);
        for _ in 0..INPUT_RESERVE {
            assert!(peer.try_send(Message::Input(InputEvent::Button { button: MouseButton::Left, down: true })));
        }
    }
}
