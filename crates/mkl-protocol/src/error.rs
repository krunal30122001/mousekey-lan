use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("encryption error: {0}")]
    Noise(#[from] snow::Error),
    #[error("message codec error: {0}")]
    Codec(#[from] bincode::Error),
    #[error("message of {0} bytes exceeds the protocol limit")]
    FrameTooLarge(usize),
    #[error("handshake failed: {0}")]
    Handshake(&'static str),
    #[error("connection closed by peer")]
    Closed,
}
