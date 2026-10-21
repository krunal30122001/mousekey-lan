//! Noise `XX` handshake and encrypted, length-prefixed framing.
//!
//! Frame format: `u16 big-endian length || Noise ciphertext`. Each direction uses its own
//! counter nonce, so replayed, reordered or tampered frames fail authentication.

use std::sync::Arc;

use snow::{Builder, StatelessTransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::messages::{decode, encode, Message};
use crate::ProtocolError;

pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"mousekey-lan/1";
const MAX_NOISE_MSG: usize = 65_535;
const TAG_LEN: usize = 16;
const READ_BUF: usize = 2 * (MAX_NOISE_MSG + 2);

type BoxRead = Box<dyn AsyncRead + Unpin + Send>;
type BoxWrite = Box<dyn AsyncWrite + Unpin + Send>;

/// Long-term Curve25519 identity of a device.
#[derive(Clone)]
pub struct Keypair {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

impl Keypair {
    pub fn generate() -> Self {
        let params = NOISE_PARAMS.parse().expect("static noise params are valid");
        let kp = Builder::new(params)
            .generate_keypair()
            .expect("system RNG available");
        Self { private: kp.private, public: kp.public }
    }
}

impl std::fmt::Debug for Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keypair")
            .field("public", &crate::fingerprint(&self.public))
            .finish_non_exhaustive()
    }
}

/// Result of a completed handshake: the authenticated remote key plus the secure channel.
pub struct Established {
    pub remote_static: Vec<u8>,
    pub handshake_hash: Vec<u8>,
    pub reader: SecureReader,
    pub writer: SecureWriter,
}

/// Runs a Noise `XX` handshake (mutual authentication with static keys) over `stream`.
pub async fn handshake<S>(
    mut stream: S,
    keys: &Keypair,
    initiator: bool,
) -> Result<Established, ProtocolError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let params = NOISE_PARAMS.parse()?;
    let builder = Builder::new(params)
        .local_private_key(&keys.private)
        .prologue(PROLOGUE);
    let mut hs = if initiator {
        builder.build_initiator()?
    } else {
        builder.build_responder()?
    };

    let mut out = vec![0u8; MAX_NOISE_MSG];
    let mut scratch = vec![0u8; MAX_NOISE_MSG];
    let mut frame = Vec::with_capacity(128);

    if initiator {
        let n = hs.write_message(&[], &mut out)?; // -> e
        write_frame(&mut stream, &out[..n]).await?;
        read_frame(&mut stream, &mut frame).await?; // <- e, ee, s, es
        hs.read_message(&frame, &mut scratch)?;
        let n = hs.write_message(&[], &mut out)?; // -> s, se
        write_frame(&mut stream, &out[..n]).await?;
    } else {
        read_frame(&mut stream, &mut frame).await?;
        hs.read_message(&frame, &mut scratch)?;
        let n = hs.write_message(&[], &mut out)?;
        write_frame(&mut stream, &out[..n]).await?;
        read_frame(&mut stream, &mut frame).await?;
        hs.read_message(&frame, &mut scratch)?;
    }

    if !hs.is_handshake_finished() {
        return Err(ProtocolError::Handshake("handshake incomplete"));
    }
    let remote_static = hs
        .get_remote_static()
        .ok_or(ProtocolError::Handshake("peer sent no static key"))?
        .to_vec();
    let handshake_hash = hs.get_handshake_hash().to_vec();
    let transport = Arc::new(hs.into_stateless_transport_mode()?);
    let (r, w) = tokio::io::split(stream);

    Ok(Established {
        remote_static,
        handshake_hash,
        reader: SecureReader::new(Box::new(r), transport.clone()),
        writer: SecureWriter::new(Box::new(w), transport),
    })
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> Result<(), ProtocolError> {
    let mut buf = Vec::with_capacity(data.len() + 2);
    buf.extend_from_slice(&(data.len() as u16).to_be_bytes());
    buf.extend_from_slice(data);
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> Result<(), ProtocolError> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await.map_err(map_eof)?;
    let n = u16::from_be_bytes(len) as usize;
    buf.resize(n, 0);
    r.read_exact(buf).await.map_err(map_eof)?;
    Ok(())
}

fn map_eof(e: std::io::Error) -> ProtocolError {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        ProtocolError::Closed
    } else {
        ProtocolError::Io(e)
    }
}

/// Receiving half of a secure channel.
pub struct SecureReader {
    inner: BoxRead,
    transport: Arc<StatelessTransportState>,
    nonce: u64,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    plain: Vec<u8>,
}

impl SecureReader {
    fn new(inner: BoxRead, transport: Arc<StatelessTransportState>) -> Self {
        Self {
            inner,
            transport,
            nonce: 0,
            buf: vec![0; READ_BUF],
            start: 0,
            end: 0,
            plain: vec![0; MAX_NOISE_MSG],
        }
    }

    /// True if another complete frame is already buffered (no I/O needed to read it).
    pub fn has_buffered_frame(&self) -> bool {
        let avail = self.end - self.start;
        avail >= 2 && {
            let len = u16::from_be_bytes([self.buf[self.start], self.buf[self.start + 1]]) as usize;
            avail >= 2 + len
        }
    }

    /// Reads, decrypts and decodes the next message.
    ///
    /// Cancel safe: partially received frames stay buffered, so this can be used in
    /// `tokio::select!` without corrupting the stream.
    pub async fn recv(&mut self) -> Result<Message, ProtocolError> {
        loop {
            let avail = self.end - self.start;
            if avail >= 2 {
                let len =
                    u16::from_be_bytes([self.buf[self.start], self.buf[self.start + 1]]) as usize;
                if avail >= 2 + len {
                    let frame_start = self.start + 2;
                    let frame = &self.buf[frame_start..frame_start + len];
                    let n = self.transport.read_message(self.nonce, frame, &mut self.plain)?;
                    self.nonce += 1;
                    self.start += 2 + len;
                    if self.start == self.end {
                        self.start = 0;
                        self.end = 0;
                    }
                    return decode(&self.plain[..n]);
                }
            }
            if self.end == self.buf.len() {
                self.buf.copy_within(self.start..self.end, 0);
                self.end -= self.start;
                self.start = 0;
            }
            let n = self.inner.read(&mut self.buf[self.end..]).await?;
            if n == 0 {
                return Err(ProtocolError::Closed);
            }
            self.end += n;
        }
    }
}

/// Sending half of a secure channel. Messages can be batched with [`push`](Self::push) and
/// written with a single syscall by [`flush`](Self::flush).
pub struct SecureWriter {
    inner: BoxWrite,
    transport: Arc<StatelessTransportState>,
    nonce: u64,
    out: Vec<u8>,
}

impl SecureWriter {
    fn new(inner: BoxWrite, transport: Arc<StatelessTransportState>) -> Self {
        Self { inner, transport, nonce: 0, out: Vec::with_capacity(4096) }
    }

    /// Encrypts `msg` into the pending output buffer.
    pub fn push(&mut self, msg: &Message) -> Result<(), ProtocolError> {
        let plain = encode(msg)?;
        if plain.len() + TAG_LEN > MAX_NOISE_MSG {
            return Err(ProtocolError::FrameTooLarge(plain.len()));
        }
        let start = self.out.len();
        self.out.resize(start + 2 + plain.len() + TAG_LEN, 0);
        let n = self
            .transport
            .write_message(self.nonce, &plain, &mut self.out[start + 2..])?;
        self.nonce += 1;
        self.out[start..start + 2].copy_from_slice(&(n as u16).to_be_bytes());
        self.out.truncate(start + 2 + n);
        Ok(())
    }

    pub async fn flush(&mut self) -> Result<(), ProtocolError> {
        if !self.out.is_empty() {
            self.inner.write_all(&self.out).await?;
            self.out.clear();
        }
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn send(&mut self, msg: &Message) -> Result<(), ProtocolError> {
        self.push(msg)?;
        self.flush().await
    }

    pub async fn close(&mut self) {
        let _ = self.inner.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{device_id, verification_code, InputEvent, Rect};
    use std::time::Duration;
    use tokio::io::duplex;

    async fn pair() -> (Established, Established, Keypair, Keypair) {
        let ka = Keypair::generate();
        let kb = Keypair::generate();
        let (a, b) = duplex(1 << 16);
        let (ka2, kb2) = (ka.clone(), kb.clone());
        let ta = tokio::spawn(async move { handshake(a, &ka2, true).await.unwrap() });
        let tb = tokio::spawn(async move { handshake(b, &kb2, false).await.unwrap() });
        (ta.await.unwrap(), tb.await.unwrap(), ka, kb)
    }

    #[tokio::test]
    async fn handshake_authenticates_both_sides() {
        let (a, b, ka, kb) = pair().await;
        assert_eq!(a.remote_static, kb.public);
        assert_eq!(b.remote_static, ka.public);
        assert_eq!(a.handshake_hash, b.handshake_hash);
        assert_eq!(verification_code(&a.handshake_hash), verification_code(&b.handshake_hash));
        assert_ne!(device_id(&ka.public), device_id(&kb.public));
    }

    #[tokio::test]
    async fn messages_flow_both_ways_in_order() {
        let (mut a, mut b, _, _) = pair().await;
        for i in 0..500 {
            a.writer.push(&Message::Input(InputEvent::MouseMove { x: i, y: -i })).unwrap();
        }
        a.writer.flush().await.unwrap();
        for i in 0..500 {
            assert_eq!(
                b.reader.recv().await.unwrap(),
                Message::Input(InputEvent::MouseMove { x: i, y: -i })
            );
        }
        b.writer.send(&Message::Pong { nonce: 42 }).await.unwrap();
        assert_eq!(a.reader.recv().await.unwrap(), Message::Pong { nonce: 42 });
    }

    #[tokio::test]
    async fn closed_stream_reports_closed() {
        let (a, mut b, _, _) = pair().await;
        drop(a);
        assert!(matches!(b.reader.recv().await, Err(ProtocolError::Closed)));
    }

    /// A relay that flips one byte of ciphertext must cause authentication failure.
    #[tokio::test]
    async fn tampered_ciphertext_is_rejected() {
        let ka = Keypair::generate();
        let kb = Keypair::generate();
        let (a, m1) = duplex(1 << 16);
        let (m2, b) = duplex(1 << 16);
        let (mut m1r, mut m1w) = tokio::io::split(m1);
        let (mut m2r, mut m2w) = tokio::io::split(m2);
        // A -> B direction: forward bytes, flipping byte 150 (inside the first transport frame).
        tokio::spawn(async move {
            let mut total = 0usize;
            let mut buf = [0u8; 4096];
            loop {
                let n = match m1r.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                for (i, byte) in buf[..n].iter_mut().enumerate() {
                    if total + i == 150 {
                        *byte ^= 0x01;
                    }
                }
                total += n;
                if m2w.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut m2r, &mut m1w).await;
        });
        let (ka2, kb2) = (ka.clone(), kb.clone());
        let ta = tokio::spawn(async move { handshake(a, &ka2, true).await.unwrap() });
        let tb = tokio::spawn(async move { handshake(b, &kb2, false).await.unwrap() });
        let mut ea = ta.await.unwrap();
        let mut eb = tb.await.unwrap();
        let big = Message::Screens(vec![Rect::new(1, 2, 3, 4); 16]);
        ea.writer.send(&big).await.unwrap();
        let res = tokio::time::timeout(Duration::from_secs(2), eb.reader.recv()).await;
        assert!(matches!(res, Ok(Err(ProtocolError::Noise(_)))), "got {res:?}");
    }
}
