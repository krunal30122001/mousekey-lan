use sha2::{Digest, Sha256};

use crate::DEFAULT_PORT;

/// Stable device identifier: the first 8 bytes of SHA-256(static public key), hex encoded.
///
/// Because it is derived from the Noise static key, a peer cannot claim another device's id
/// without also holding that device's private key.
pub fn device_id(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    hex::encode(&digest[..8])
}

/// Human-readable key fingerprint, e.g. `3F2A-91C0-...`, shown in the UI for manual checks.
pub fn fingerprint(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    let hex = hex::encode_upper(&digest[..12]);
    hex.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

/// Six-digit code derived from the Noise handshake hash.
///
/// Both ends of one handshake compute the same value; a man-in-the-middle would produce two
/// different handshakes and therefore two different codes. Users compare the codes on both
/// screens before approving a pairing.
pub fn verification_code(handshake_hash: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"mousekey-lan/pair-code");
    h.update(handshake_hash);
    let d = h.finalize();
    let v = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{:03} {:03}", v / 1000, v % 1000)
}

/// Where to connect for a manual pairing, parsed from user input or a QR / pairing link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairTarget {
    pub host: String,
    pub port: u16,
    /// When present (from a QR code / link), the connection is aborted unless the remote's
    /// authenticated key hashes to this id.
    pub device_id: Option<String>,
}

/// Builds a pairing link such as `mkl://192.168.1.20:47820?id=0123456789abcdef`.
pub fn pair_uri(host: &str, port: u16, device_id: &str) -> String {
    if host.contains(':') {
        format!("mkl://[{host}]:{port}?id={device_id}")
    } else {
        format!("mkl://{host}:{port}?id={device_id}")
    }
}

/// Accepts `host`, `host:port`, `[v6]:port` or a full `mkl://` link.
pub fn parse_pair_target(input: &str) -> Option<PairTarget> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let (body, query) = match s.strip_prefix("mkl://") {
        Some(rest) => match rest.split_once('?') {
            Some((b, q)) => (b, Some(q)),
            None => (rest, None),
        },
        None => (s, None),
    };
    let body = body.trim_end_matches('/');
    let device_id = query
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("id=")))
        .map(|v| v.to_ascii_lowercase())
        .filter(|v| v.len() == 16 && v.chars().all(|c| c.is_ascii_hexdigit()));

    let (host, port) = if let Some(rest) = body.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => DEFAULT_PORT,
        };
        (h.to_string(), port)
    } else {
        match body.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().ok()?),
            _ => (body.to_string(), DEFAULT_PORT),
        }
    };
    if host.is_empty() || host.chars().any(char::is_whitespace) || port == 0 {
        return None;
    }
    Some(PairTarget { host, port, device_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = device_id(&[1u8; 32]);
        assert_eq!(a, device_id(&[1u8; 32]));
        assert_ne!(a, device_id(&[2u8; 32]));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn code_format() {
        let c = verification_code(&[9u8; 32]);
        assert_eq!(c.len(), 7);
        assert_eq!(&c[3..4], " ");
    }

    #[test]
    fn parse_targets() {
        assert_eq!(
            parse_pair_target("192.168.1.5"),
            Some(PairTarget { host: "192.168.1.5".into(), port: DEFAULT_PORT, device_id: None })
        );
        assert_eq!(parse_pair_target(" 10.0.0.2:5000 ").unwrap().port, 5000);
        let t = parse_pair_target("mkl://192.168.1.9:47820?id=0123456789ABCDEF").unwrap();
        assert_eq!(t.host, "192.168.1.9");
        assert_eq!(t.device_id.as_deref(), Some("0123456789abcdef"));
        assert_eq!(parse_pair_target("[fe80::1]:99").unwrap().host, "fe80::1");
        assert_eq!(parse_pair_target("fe80::1").unwrap().port, DEFAULT_PORT);
        assert!(parse_pair_target("").is_none());
        assert!(parse_pair_target("host:notaport").is_none());
        let uri = pair_uri("192.168.1.9", 47820, "0123456789abcdef");
        assert_eq!(parse_pair_target(&uri).unwrap().device_id.as_deref(), Some("0123456789abcdef"));
    }
}
