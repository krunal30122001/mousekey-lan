use bincode::Options;
use serde::{Deserialize, Serialize};

use crate::ProtocolError;

/// Bumped whenever the message set changes incompatibly.
pub const PROTOCOL_VERSION: u16 = 3;
/// Default TCP port the engine listens on.
pub const DEFAULT_PORT: u16 = 47820;
/// mDNS / DNS-SD service type used for LAN discovery.
pub const SERVICE_TYPE: &str = "_mousekeylan._tcp.local.";
/// Largest plaintext message accepted on the wire (well below Noise's 65 535 byte limit).
pub const MAX_PLAINTEXT: usize = 60 * 1024;
/// Upper bound on monitors accepted from a peer.
pub const MAX_SCREENS: usize = 16;

/// Reject reason sent when the remote device is not (or no longer) trusted.
pub const REJECT_NOT_TRUSTED: &str = "not_trusted";

/// A rectangle in physical pixels, in the owning device's virtual-desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Exclusive right edge.
    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    /// Exclusive bottom edge.
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Clamps a point so it lies inside this rectangle. Requires `w > 0 && h > 0`.
    pub fn clamp(&self, x: i32, y: i32) -> (i32, i32) {
        (
            x.clamp(self.x, self.right() - 1),
            y.clamp(self.y, self.bottom() - 1),
        )
    }

    /// Squared distance from a point to the nearest pixel of this rectangle.
    pub fn distance_sq(&self, x: i32, y: i32) -> i64 {
        let (cx, cy) = self.clamp(x, y);
        let dx = (x - cx) as i64;
        let dy = (y - cy) as i64;
        dx * dx + dy * dy
    }

    pub fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Smallest rectangle containing every rectangle in `rects`.
    pub fn bounding(rects: &[Rect]) -> Option<Rect> {
        let first = rects.first()?;
        let (mut l, mut t, mut r, mut b) = (first.x, first.y, first.right(), first.bottom());
        for rc in &rects[1..] {
            l = l.min(rc.x);
            t = t.min(rc.y);
            r = r.max(rc.right());
            b = b.max(rc.bottom());
        }
        Some(Rect::new(l, t, r - l, b - t))
    }
}

/// Drops degenerate or absurd monitor rectangles received from a peer.
pub fn sanitize_screens(screens: Vec<Rect>) -> Vec<Rect> {
    const LIMIT: i32 = 1 << 20;
    screens
        .into_iter()
        .filter(|r| {
            r.w > 0
                && r.h > 0
                && r.w <= 32_768
                && r.h <= 32_768
                && r.x.abs() < LIMIT
                && r.y.abs() < LIMIT
        })
        .take(MAX_SCREENS)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

/// An input event to be injected on the receiving device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Absolute cursor position in the receiver's virtual-desktop pixels.
    MouseMove { x: i32, y: i32 },
    Button { button: MouseButton, down: bool },
    /// Wheel delta in Windows units (120 per notch).
    Wheel { delta: i32, horizontal: bool },
    /// Physical key identified primarily by scan code (layout independent); `vk` is a fallback.
    Key {
        vk: u16,
        scan: u16,
        extended: bool,
        down: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u16,
    pub device_id: String,
    pub name: String,
    pub app_version: String,
    pub listen_port: u16,
    pub screens: Vec<Rect>,
    /// True when the connecting user explicitly asked to pair with this device.
    pub pairing: bool,
}

/// Shared monitor arrangement. Positions are in layout pixels; the newest revision wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutSync {
    pub revision: u64,
    pub positions: Vec<(String, i32, i32)>,
}

/// Largest chunk of file / clipboard data per message.
pub const TRANSFER_CHUNK: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferKind {
    /// UTF-8 text for the clipboard.
    ClipboardText,
    /// A device-independent bitmap (CF_DIB) for the clipboard.
    ClipboardImage,
    /// Files copied to the clipboard; the receiver offers them for paste.
    ClipboardFiles,
    /// Files dragged across a screen edge; the receiver saves and shows them.
    DropFiles,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Hello(Hello),
    /// This side authorizes the session.
    Accept,
    Reject { reason: String },
    Ping { nonce: u64 },
    Pong { nonce: u64 },
    /// The sender takes keyboard/mouse control of the receiver; cursor enters at `(x, y)`.
    ControlEnter { x: i32, y: i32 },
    /// The sender gives control back; the receiver must release anything still pressed.
    ControlLeave,
    /// The receiver's own user touched its physical input, so it reclaims control.
    ControlRevoked,
    Input(InputEvent),
    Screens(Vec<Rect>),
    Layout(LayoutSync),
    Status { locked: bool },
    Bye,
    /// Starts a clipboard or file transfer. Followed by `TransferFile`/`TransferChunk`
    /// messages (chunks belong to the most recent file) and a final `TransferEnd`.
    TransferStart { id: u64, kind: TransferKind, total_bytes: u64, file_count: u32 },
    /// Next item of a transfer; `path` is relative and `/`-separated (empty for clipboard data).
    TransferFile { id: u64, path: String, size: u64 },
    TransferChunk { id: u64, data: Vec<u8> },
    TransferEnd { id: u64 },
    TransferCancel { id: u64, reason: String },
    /// A user-facing warning from the peer (e.g. its focused window blocks remote typing).
    Notice { text: String },
}

fn codec() -> impl Options {
    bincode::DefaultOptions::new().with_limit(MAX_PLAINTEXT as u64)
}

pub fn encode(msg: &Message) -> Result<Vec<u8>, ProtocolError> {
    let bytes = codec().serialize(msg)?;
    if bytes.len() > MAX_PLAINTEXT {
        return Err(ProtocolError::FrameTooLarge(bytes.len()));
    }
    Ok(bytes)
}

pub fn decode(bytes: &[u8]) -> Result<Message, ProtocolError> {
    if bytes.len() > MAX_PLAINTEXT {
        return Err(ProtocolError::FrameTooLarge(bytes.len()));
    }
    Ok(codec().deserialize(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_messages() {
        let msgs = vec![
            Message::Hello(Hello {
                protocol: PROTOCOL_VERSION,
                device_id: "0123456789abcdef".into(),
                name: "Desk PC".into(),
                app_version: "0.1.0".into(),
                listen_port: DEFAULT_PORT,
                screens: vec![Rect::new(0, 0, 1920, 1080), Rect::new(-1280, 0, 1280, 1024)],
                pairing: true,
            }),
            Message::Accept,
            Message::Reject { reason: "nope".into() },
            Message::Ping { nonce: u64::MAX },
            Message::Pong { nonce: 7 },
            Message::ControlEnter { x: -5, y: 900 },
            Message::ControlLeave,
            Message::ControlRevoked,
            Message::Input(InputEvent::MouseMove { x: 10, y: -20 }),
            Message::Input(InputEvent::Button { button: MouseButton::X2, down: true }),
            Message::Input(InputEvent::Wheel { delta: -240, horizontal: false }),
            Message::Input(InputEvent::Key { vk: 0x41, scan: 0x1E, extended: false, down: true }),
            Message::Screens(vec![Rect::new(0, 0, 800, 600)]),
            Message::Layout(LayoutSync { revision: 3, positions: vec![("a".into(), 1, 2)] }),
            Message::Status { locked: true },
            Message::Bye,
            Message::TransferStart { id: 9, kind: TransferKind::DropFiles, total_bytes: 1 << 40, file_count: 3 },
            Message::TransferFile { id: 9, path: "dir/a b.txt".into(), size: 12 },
            Message::TransferChunk { id: 9, data: vec![7; TRANSFER_CHUNK] },
            Message::TransferEnd { id: 9 },
            Message::TransferCancel { id: 9, reason: "x".into() },
            Message::Notice { text: "hi".into() },
        ];
        for m in msgs {
            let bytes = encode(&m).unwrap();
            assert_eq!(decode(&bytes).unwrap(), m);
        }
    }

    #[test]
    fn mouse_moves_are_compact() {
        let bytes = encode(&Message::Input(InputEvent::MouseMove { x: 1919, y: 1079 })).unwrap();
        assert!(bytes.len() <= 8, "mouse move encoded to {} bytes", bytes.len());
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(&[0xff, 0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn rect_helpers() {
        let r = Rect::new(-1920, 0, 1920, 1080);
        assert!(r.contains(-1, 0));
        assert!(!r.contains(0, 0));
        assert_eq!(r.clamp(100, -50), (-1, 0));
        let b = Rect::bounding(&[r, Rect::new(0, 0, 2560, 1440)]).unwrap();
        assert_eq!(b, Rect::new(-1920, 0, 4480, 1440));
    }

    #[test]
    fn sanitize_drops_bad_rects() {
        let s = sanitize_screens(vec![Rect::new(0, 0, 0, 10), Rect::new(0, 0, 10, 10), Rect::new(0, 0, 99_999, 10)]);
        assert_eq!(s, vec![Rect::new(0, 0, 10, 10)]);
    }
}
