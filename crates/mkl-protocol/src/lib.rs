//! Wire protocol, encrypted transport and pairing helpers for MouseKey LAN.
//!
//! * [`messages`] — the message set exchanged between devices and its compact binary codec.
//! * [`transport`] — a Noise `XX` handshake plus length-prefixed, encrypted framing over any
//!   async byte stream (TCP in production, in-memory pipes in tests).
//! * [`pairing`] — device identity derivation, human-verifiable pairing codes and pairing links.

mod error;
pub mod messages;
pub mod pairing;
pub mod transport;

pub use error::ProtocolError;
pub use messages::*;
pub use pairing::*;
pub use transport::{handshake, Established, Keypair, SecureReader, SecureWriter};
