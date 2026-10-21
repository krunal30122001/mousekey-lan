//! The MouseKey LAN engine.
//!
//! ```text
//!  hook thread ──► router ──► PeerConn.tx (bounded) ──► writer task ──► TCP/Noise ──► peer
//!                                                                                      │
//!  injector ◄── InjectState ◄── dispatch ◄── reader task ◄── TCP/Noise ◄───────────────┘
//! ```
//!
//! * [`router`] runs synchronously inside the OS input hook and decides whether each event
//!   stays local or is forwarded to the device currently under control.
//! * [`net`] owns connections: handshake, pairing authorization, heartbeats, dispatch.
//! * [`reconnect`] re-dials trusted devices with exponential backoff.
//! * [`layout`] holds the monitor arrangement and the screen-edge geometry.
//! * [`Engine`] is the façade used by the desktop shell (Tauri) and by tests.

mod engine;
mod inject;
pub mod layout;
mod net;
mod reconnect;
mod router;
mod settings;
mod transfer;
mod state;
mod views;

pub use engine::{Engine, EngineConfig};
pub use settings::Settings;
pub use views::*;
