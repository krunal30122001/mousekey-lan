//! Keeps trusted devices connected: re-dials with exponential backoff (capped at 30 s) and
//! reacts immediately to discovery, unlock, resume and disconnect notifications.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mkl_store::DeviceRecord;
use tracing::debug;

use crate::net::{self, DialIntent};
use crate::state::{Backoff, Shared};

const TICK: Duration = Duration::from_secs(2);

/// The device with the lower id dials first; the other waits a little longer so that both
/// sides rarely race to open duplicate connections.
fn base_delay(me: &str, other: &str) -> Duration {
    if me < other {
        Duration::from_millis(300)
    } else {
        Duration::from_millis(1500)
    }
}

fn candidates(shared: &Shared, dev: &DeviceRecord) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = shared
        .discovered
        .read()
        .get(&dev.device_id)
        .map(|d| d.addrs.clone())
        .unwrap_or_default();
    if let Some(addr) = dev.last_addr.as_deref().and_then(|a| a.parse().ok()) {
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    out
}

pub async fn run(shared: Arc<Shared>) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = shared.reconnect_now.notified() => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        if !shared.settings.read().auto_reconnect {
            continue;
        }
        let Ok(devices) = shared.store.list_devices() else { continue };
        for dev in devices.into_iter().filter(|d| d.trusted && !d.blocked) {
            let id = dev.device_id.clone();
            if shared.peers.read().contains_key(&id)
                || shared.paused.lock().contains(&id)
                || shared.dialing.lock().contains(&id)
            {
                continue;
            }
            {
                let mut backoff = shared.backoff.lock();
                let b = backoff.entry(id.clone()).or_insert_with(|| Backoff::new(base_delay(&shared.me_id, &id)));
                if b.next > Instant::now() {
                    continue;
                }
            }
            let addrs = candidates(&shared, &dev);
            if addrs.is_empty() {
                continue;
            }
            shared.dialing.lock().insert(id.clone());
            shared.changed();
            let s = shared.clone();
            shared.rt.spawn(async move {
                let mut connected = false;
                for addr in addrs {
                    let intent = DialIntent { pairing: false, expected_id: Some(id.clone()) };
                    match net::dial(s.clone(), addr, intent).await {
                        Ok(()) => {
                            connected = true;
                            break;
                        }
                        Err(e) => debug!(device = %id, %addr, "reconnect attempt failed: {e:#}"),
                    }
                }
                s.dialing.lock().remove(&id);
                {
                    let mut backoff = s.backoff.lock();
                    let b = backoff.entry(id.clone()).or_insert_with(|| Backoff::new(base_delay(&s.me_id, &id)));
                    if connected {
                        b.reset();
                    } else {
                        b.fail();
                    }
                }
                s.changed();
            });
        }
    }
}
