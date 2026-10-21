//! Serializable types exchanged with the desktop shell / frontend.

use mkl_protocol::Rect;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EngineEvent {
    /// Something in the snapshot changed; the UI should refresh.
    Changed,
    /// Another device asks to pair with this one. Codes must match on both screens.
    PairRequest { request_id: u64, device_id: String, name: String, addr: String, code: String },
    /// A pairing request was answered, cancelled or timed out.
    PairRequestClosed { request_id: u64 },
    /// We asked to pair and are waiting for the other device's user to approve.
    PairPending { device_id: String, name: String, code: String },
    PairResult { device_id: Option<String>, name: String, success: bool, message: String },
    /// Device this machine's keyboard/mouse is currently driving (`None` = local).
    ActiveChanged { active: Option<String> },
    Notice { level: String, message: String },
    /// Progress of a clipboard / file transfer (`state`: active, done, failed).
    Transfer {
        id: u64,
        peer: String,
        outgoing: bool,
        kind: String,
        label: String,
        done: u64,
        total: u64,
        state: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeView {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub addresses: Vec<String>,
    pub fingerprint: String,
    pub pair_uri: String,
    pub screens: Vec<Rect>,
    pub version: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DeviceStatus {
    Online,
    Connecting,
    Offline,
    Discovered,
    Blocked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub status: DeviceStatus,
    pub trusted: bool,
    pub blocked: bool,
    pub paused: bool,
    pub addr: Option<String>,
    pub last_seen: Option<i64>,
    pub paired_at: Option<i64>,
    pub rtt_ms: Option<f64>,
    pub screens: Vec<Rect>,
    pub remote_locked: bool,
    pub fingerprint: Option<String>,
    /// This machine is driving that device.
    pub controlling: bool,
    /// That device is driving this machine.
    pub controlling_us: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub me: MeView,
    pub devices: Vec<DeviceView>,
    pub active_target: Option<String>,
    pub controlled_by: Option<String>,
    pub locked: bool,
    pub capture_running: bool,
    pub discovery_running: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutDevice {
    pub id: String,
    pub name: String,
    pub is_local: bool,
    pub online: bool,
    pub x: i32,
    pub y: i32,
    pub screens: Vec<Rect>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutView {
    pub revision: u64,
    pub devices: Vec<LayoutDevice>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutPos {
    pub id: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerDiag {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub rtt_ms: Option<f64>,
    pub quality: String,
    pub connected_secs: u64,
    pub sent: u64,
    pub received: u64,
    pub dropped: u64,
    pub remote_locked: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub device_id: String,
    pub uptime_secs: u64,
    pub listen_port: u16,
    pub capture_running: bool,
    pub discovery_running: bool,
    pub locked: bool,
    pub hook_events: u64,
    pub hook_avg_us: f64,
    pub hook_max_us: f64,
    pub forwarded: u64,
    pub injected: u64,
    pub inject_errors: u64,
    pub unauthorized: u64,
    pub switches: u64,
    pub reconnects: u64,
    pub last_reconnect_ms: Option<u64>,
    pub keys_held_by_remote: usize,
    pub screens: Vec<Rect>,
    pub peers: Vec<PeerDiag>,
}

pub fn quality_for(rtt_ms: Option<f64>) -> &'static str {
    match rtt_ms {
        None => "measuring",
        Some(r) if r < 5.0 => "excellent",
        Some(r) if r < 15.0 => "good",
        Some(r) if r < 40.0 => "fair",
        Some(_) => "poor",
    }
}
