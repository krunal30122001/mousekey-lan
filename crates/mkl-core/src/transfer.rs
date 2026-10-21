//! Clipboard sharing and file transfer.
//!
//! * Text and images are pushed to every connected PC as soon as they are copied.
//! * Copied files are sent lazily: only when control moves to another PC (or back to the
//!   PC that was controlling this one), so copying a big folder costs nothing until the
//!   user actually goes to paste it. The receiver puts the files on its clipboard.
//! * Files dragged across a screen edge are sent on release and saved to the receive folder
//!   (Downloads\MouseKey LAN), which is then shown in Explorer.
//!
//! Data travels on each connection's low-priority bulk queue, so input stays responsive
//! during transfers. Loops (A sets B's clipboard, B "sees" a change and sends it back) are
//! prevented by remembering a hash of the last content synced.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use mkl_input::ClipContent;
use mkl_protocol::{Message, TransferKind, TRANSFER_CHUNK};
use tokio::io::AsyncReadExt;
use tracing::{info, warn};

use crate::state::{PeerConn, Shared};
use crate::views::EngineEvent;

const MAX_TEXT: u64 = 16 * 1024 * 1024;
const MAX_IMAGE: u64 = 128 * 1024 * 1024;
const MAX_FILES: usize = 20_000;
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

#[derive(Default)]
pub struct ClipState {
    /// Clipboard sequence number last processed.
    last_seq: Option<u32>,
    /// Hash of the clipboard content most recently sent or received (loop prevention).
    last_hash: Option<u64>,
    /// Files copied on this PC that haven't been sent yet.
    pending_files: Option<PendingFiles>,
}

struct PendingFiles {
    paths: Vec<PathBuf>,
    sent_to: HashSet<String>,
}

pub struct Incoming {
    kind: TransferKind,
    from: String,
    root: PathBuf,
    total: u64,
    received: u64,
    file_count: u32,
    data: Vec<u8>,
    current: Option<File>,
    current_path: Option<PathBuf>,
    /// Top-level items written (what goes on the clipboard / gets revealed).
    top_level: Vec<PathBuf>,
    renames: HashMap<String, String>,
    last_progress: Instant,
}

fn hash_of(c: &ClipContent) -> u64 {
    let mut h = DefaultHasher::new();
    match c {
        ClipContent::Text(t) => (0u8, t).hash(&mut h),
        ClipContent::Image(b) => (1u8, b).hash(&mut h),
        ClipContent::Files(f) => (2u8, f).hash(&mut h),
    }
    h.finish()
}

fn kind_label(kind: TransferKind) -> &'static str {
    match kind {
        TransferKind::ClipboardText => "Clipboard text",
        TransferKind::ClipboardImage => "Clipboard image",
        TransferKind::ClipboardFiles => "Copied files",
        TransferKind::DropFiles => "Dropped files",
    }
}

fn is_file_kind(kind: TransferKind) -> bool {
    matches!(kind, TransferKind::ClipboardFiles | TransferKind::DropFiles)
}

#[allow(clippy::too_many_arguments)]
fn progress(s: &Shared, id: u64, peer: &str, outgoing: bool, kind: TransferKind, label: &str, done: u64, total: u64, state: &str) {
    s.emit(EngineEvent::Transfer {
        id,
        peer: peer.to_string(),
        outgoing,
        kind: kind_label(kind).to_string(),
        label: label.to_string(),
        done,
        total,
        state: state.to_string(),
    });
}

// ---------------------------------------------------------------------------------------
// Local clipboard changes
// ---------------------------------------------------------------------------------------

pub fn on_clipboard_changed(shared: &Arc<Shared>) {
    let s = shared.clone();
    shared.rt.spawn(async move { refresh_clipboard(&s).await });
}

/// Reads the clipboard if it changed since last processed, then shares text / images and
/// remembers copied files. Serialized so concurrent callers can't process one copy twice.
async fn refresh_clipboard(s: &Arc<Shared>) {
    let _serial = s.clip_refresh.lock().await;
    let (share_clip, share_files) = {
        let st = s.settings.read();
        (st.share_clipboard, st.share_files)
    };
    if !share_clip && !share_files {
        return;
    }
    let seq = s.desktop.clipboard_sequence();
    if s.clip.lock().last_seq == Some(seq) {
        return;
    }
    let desktop = s.desktop.clone();
    let content = tokio::task::spawn_blocking(move || desktop.read_clipboard()).await.ok().flatten();
    s.clip.lock().last_seq = Some(seq);
    let Some(content) = content else { return };
    let hash = hash_of(&content);
    {
        let mut clip = s.clip.lock();
        if clip.last_hash == Some(hash) {
            return; // content we just received from (or sent to) another PC
        }
        clip.last_hash = Some(hash);
        clip.pending_files = None;
    }
    match content {
        ClipContent::Text(t) if share_clip && (t.len() as u64) <= MAX_TEXT => {
            broadcast_bytes(s, TransferKind::ClipboardText, t.into_bytes());
        }
        ClipContent::Image(b) if share_clip && (b.len() as u64) <= MAX_IMAGE => {
            broadcast_bytes(s, TransferKind::ClipboardImage, b);
        }
        ClipContent::Files(paths) if share_files => {
            s.clip.lock().pending_files = Some(PendingFiles { paths, sent_to: HashSet::new() });
        }
        _ => {}
    }
}

fn broadcast_bytes(s: &Arc<Shared>, kind: TransferKind, data: Vec<u8>) {
    let data = Arc::new(data);
    let peers: Vec<_> = s.peers.read().values().cloned().collect();
    for peer in peers {
        let (s2, d) = (s.clone(), data.clone());
        s.rt.spawn(async move {
            if let Err(e) = send_bytes(&s2, &peer, kind, &d).await {
                warn!(device = %peer.id, "clipboard send failed: {e:#}");
            }
        });
    }
}

async fn send_bytes(s: &Shared, peer: &PeerConn, kind: TransferKind, data: &[u8]) -> Result<()> {
    let id = s.next_id();
    let total = data.len() as u64;
    let tx = &peer.bulk_tx;
    tx.send(Message::TransferStart { id, kind, total_bytes: total, file_count: 1 }).await?;
    tx.send(Message::TransferFile { id, path: String::new(), size: total }).await?;
    for chunk in data.chunks(TRANSFER_CHUNK) {
        tx.send(Message::TransferChunk { id, data: chunk.to_vec() }).await?;
    }
    tx.send(Message::TransferEnd { id }).await?;
    Ok(())
}

/// Control just moved to `peer_id` (this PC is the controller): deliver copied files.
pub fn on_control_entered(shared: &Arc<Shared>, peer_id: &str) {
    refresh_then_send(shared, peer_id);
}

/// The controller `peer_id` gave control back: deliver files copied here meanwhile.
pub fn on_control_left(shared: &Arc<Shared>, peer_id: &str) {
    refresh_then_send(shared, peer_id);
}

/// The copy may have happened a moment ago (Ctrl+C, then straight across the edge), before
/// its change notification was processed: re-check the clipboard first.
fn refresh_then_send(shared: &Arc<Shared>, peer_id: &str) {
    let s = shared.clone();
    let id = peer_id.to_string();
    shared.rt.spawn(async move {
        // Give Explorer a moment to finish publishing the copied files.
        tokio::time::sleep(Duration::from_millis(150)).await;
        refresh_clipboard(&s).await;
        send_pending_files_to(&s, &id);
    });
}

fn send_pending_files_to(shared: &Arc<Shared>, peer_id: &str) {
    if !shared.settings.read().share_files {
        return;
    }
    let paths = {
        let mut clip = shared.clip.lock();
        let Some(p) = clip.pending_files.as_mut() else { return };
        if !p.sent_to.insert(peer_id.to_string()) {
            return;
        }
        p.paths.clone()
    };
    if let Some(peer) = shared.peer(peer_id) {
        send_files(shared, peer, TransferKind::ClipboardFiles, paths);
    }
}

// ---------------------------------------------------------------------------------------
// Sending files
// ---------------------------------------------------------------------------------------

/// Expands files and folders into `(relative path, absolute path, size)` entries.
fn collect(paths: &[PathBuf]) -> Result<Vec<(String, PathBuf, u64)>> {
    fn walk(rel: String, abs: &Path, out: &mut Vec<(String, PathBuf, u64)>) -> Result<()> {
        if out.len() > MAX_FILES {
            bail!("too many files (more than {MAX_FILES})");
        }
        let meta = std::fs::metadata(abs).with_context(|| format!("cannot read {}", abs.display()))?;
        if meta.is_dir() {
            for entry in std::fs::read_dir(abs)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                walk(format!("{rel}/{name}"), &entry.path(), out)?;
            }
        } else if meta.is_file() {
            out.push((rel, abs.to_path_buf(), meta.len()));
        }
        Ok(())
    }
    let mut out = Vec::new();
    for p in paths {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "item".into());
        walk(name, p, &mut out)?;
    }
    Ok(out)
}

pub fn send_files(shared: &Arc<Shared>, peer: Arc<PeerConn>, kind: TransferKind, paths: Vec<PathBuf>) {
    let s = shared.clone();
    shared.rt.spawn(async move {
        let id = s.next_id();
        let label = match paths.as_slice() {
            [one] => one.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            many => format!("{} items", many.len()),
        };
        if let Err(e) = send_files_inner(&s, &peer, id, kind, paths, &label).await {
            warn!(device = %peer.id, "file transfer failed: {e:#}");
            let _ = peer.bulk_tx.send(Message::TransferCancel { id, reason: e.to_string() }).await;
            progress(&s, id, &peer.name, true, kind, &label, 0, 0, "failed");
            s.emit(EngineEvent::Notice { level: "warn".into(), message: format!("Sending {label} to {} failed: {e}", peer.name) });
        }
    });
}

async fn send_files_inner(s: &Shared, peer: &PeerConn, id: u64, kind: TransferKind, paths: Vec<PathBuf>, label: &str) -> Result<()> {
    let entries = tokio::task::spawn_blocking(move || collect(&paths)).await??;
    if entries.is_empty() {
        bail!("nothing to send (only empty folders)");
    }
    let total: u64 = entries.iter().map(|e| e.2).sum();
    let limit = s.settings.read().max_transfer_mb * 1024 * 1024;
    if total > limit {
        bail!("{} MB exceeds the transfer limit of {} MB (Settings)", total / (1024 * 1024), limit / (1024 * 1024));
    }
    info!(device = %peer.id, files = entries.len(), bytes = total, ?kind, "sending files");
    let tx = &peer.bulk_tx;
    tx.send(Message::TransferStart { id, kind, total_bytes: total, file_count: entries.len() as u32 }).await?;
    let mut done = 0u64;
    let mut last = Instant::now();
    progress(s, id, &peer.name, true, kind, label, 0, total, "active");
    let mut buf = vec![0u8; TRANSFER_CHUNK];
    for (rel, abs, size) in entries {
        tx.send(Message::TransferFile { id, path: rel, size }).await?;
        let mut f = tokio::fs::File::open(&abs).await.with_context(|| format!("cannot open {}", abs.display()))?;
        let mut left = size;
        while left > 0 {
            let want = (left as usize).min(TRANSFER_CHUNK);
            let n = f.read(&mut buf[..want]).await?;
            if n == 0 {
                bail!("{} changed while being sent", abs.display());
            }
            if s.cancelled_out.lock().remove(&id) {
                bail!("cancelled by {}", peer.name);
            }
            tx.send(Message::TransferChunk { id, data: buf[..n].to_vec() }).await?;
            left -= n as u64;
            done += n as u64;
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                progress(s, id, &peer.name, true, kind, label, done, total, "active");
            }
        }
    }
    tx.send(Message::TransferEnd { id }).await?;
    progress(s, id, &peer.name, true, kind, label, total, total, "done");
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Receiving
// ---------------------------------------------------------------------------------------

/// Makes a relative `/`-separated path from a peer safe to create under the receive folder.
pub fn sanitize_rel(path: &str) -> Option<PathBuf> {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
        "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let mut out = PathBuf::new();
    for comp in path.split(['/', '\\']) {
        match comp {
            "" | "." => continue,
            ".." => return None,
            _ => {}
        }
        let mut clean: String = comp
            .chars()
            .map(|c| if c.is_control() || r#"<>:"|?*"#.contains(c) { '_' } else { c })
            .collect();
        clean = clean.trim_end_matches(['.', ' ']).to_string();
        if clean.is_empty() {
            return None;
        }
        let stem = clean.split('.').next().unwrap_or("").to_ascii_uppercase();
        if RESERVED.contains(&stem.as_str()) {
            clean.insert(0, '_');
        }
        out.push(clean);
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// `name`, or `name (1)`, `name (2)`… — whichever does not exist in `dir` yet.
fn unique_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..10_000)
        .map(|i| format!("{stem} ({i}){ext}"))
        .find(|n| !dir.join(n).exists())
        .unwrap_or_else(|| format!("{stem} ({}){ext}", std::process::id()))
}

pub fn on_message(shared: &Arc<Shared>, peer: &Arc<PeerConn>, msg: Message) {
    let id = match &msg {
        Message::TransferStart { id, .. }
        | Message::TransferFile { id, .. }
        | Message::TransferChunk { id, .. }
        | Message::TransferEnd { id }
        | Message::TransferCancel { id, .. } => *id,
        _ => return,
    };
    let key = (peer.id.clone(), id);
    if shared.refused.lock().contains(&key) {
        // Already cancelled: drop the rest of its data silently.
        if matches!(msg, Message::TransferEnd { .. } | Message::TransferCancel { .. }) {
            shared.refused.lock().remove(&key);
        }
        return;
    }
    let is_cancel = matches!(msg, Message::TransferCancel { .. });
    if let Err(e) = handle(shared, peer, &key, msg) {
        if is_cancel {
            return;
        }
        warn!(device = %peer.id, "incoming transfer failed: {e:#}");
        shared.refused.lock().insert(key.clone());
        if let Some(inc) = shared.incoming.lock().remove(&key) {
            cleanup_partial(&inc);
            progress(shared, id, &peer.name, false, inc.kind, &inc.from, inc.received, inc.total, "failed");
        }
        peer.try_send(Message::TransferCancel { id, reason: e.to_string() });
    }
}

fn cleanup_partial(inc: &Incoming) {
    if let Some(p) = &inc.current_path {
        let _ = std::fs::remove_file(p);
    }
}

fn handle(s: &Arc<Shared>, peer: &Arc<PeerConn>, key: &(String, u64), msg: Message) -> Result<()> {
    match msg {
        Message::TransferStart { id, kind, total_bytes, file_count } => {
            let settings = s.settings.read().clone();
            let allowed = match kind {
                TransferKind::ClipboardText => settings.share_clipboard && total_bytes <= MAX_TEXT,
                TransferKind::ClipboardImage => settings.share_clipboard && total_bytes <= MAX_IMAGE,
                _ => {
                    settings.share_files
                        && total_bytes <= settings.max_transfer_mb * 1024 * 1024
                        && (file_count as usize) <= MAX_FILES
                }
            };
            if !allowed {
                bail!("transfer refused by settings or size limit");
            }
            let root = match kind {
                TransferKind::DropFiles => s.receive_dir.clone(),
                TransferKind::ClipboardFiles => s.temp_dir.join("clipboard").join(format!("{}-{id}", now_secs())),
                _ => PathBuf::new(),
            };
            if is_file_kind(kind) {
                std::fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;
            }
            let inc = Incoming {
                kind,
                from: peer.name.clone(),
                root,
                total: total_bytes,
                received: 0,
                file_count,
                data: Vec::new(),
                current: None,
                current_path: None,
                top_level: Vec::new(),
                renames: HashMap::new(),
                last_progress: Instant::now(),
            };
            if is_file_kind(kind) {
                progress(s, id, &peer.name, false, kind, &peer.name, 0, total_bytes, "active");
            }
            s.incoming.lock().insert(key.clone(), inc);
        }
        Message::TransferFile { path, size, .. } => {
            let mut map = s.incoming.lock();
            let inc = map.get_mut(key).context("unknown transfer")?;
            if !is_file_kind(inc.kind) {
                inc.data.reserve(size.min(inc.total) as usize);
                return Ok(());
            }
            let rel = sanitize_rel(&path).context("invalid file name")?;
            let mut comps = rel.components();
            let first = comps.next().context("empty path")?.as_os_str().to_string_lossy().to_string();
            let renamed = match inc.renames.get(&first) {
                Some(r) => r.clone(),
                None => {
                    let r = unique_name(&inc.root, &first);
                    inc.renames.insert(first.clone(), r.clone());
                    inc.top_level.push(inc.root.join(&r));
                    r
                }
            };
            let rest = comps.as_path();
            let mut dest = inc.root.join(renamed);
            if !rest.as_os_str().is_empty() {
                dest.push(rest);
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            inc.current = Some(File::create(&dest).with_context(|| format!("cannot create {}", dest.display()))?);
            inc.current_path = Some(dest);
        }
        Message::TransferChunk { id, data } => {
            let mut map = s.incoming.lock();
            let inc = map.get_mut(key).context("unknown transfer")?;
            inc.received += data.len() as u64;
            if inc.received > inc.total {
                bail!("peer sent more data than announced");
            }
            if is_file_kind(inc.kind) {
                inc.current.as_mut().context("data before file header")?.write_all(&data)?;
                if inc.last_progress.elapsed() >= PROGRESS_EVERY {
                    inc.last_progress = Instant::now();
                    progress(s, id, &inc.from, false, inc.kind, &inc.from, inc.received, inc.total, "active");
                }
            } else {
                inc.data.extend_from_slice(&data);
            }
        }
        Message::TransferEnd { id } => {
            let inc = s.incoming.lock().remove(key).context("unknown transfer")?;
            finish(s, id, inc);
        }
        Message::TransferCancel { id, reason } => {
            if let Some(inc) = s.incoming.lock().remove(key) {
                cleanup_partial(&inc);
                progress(s, id, &peer.name, false, inc.kind, &inc.from, inc.received, inc.total, "failed");
                info!(device = %peer.id, "transfer cancelled by peer: {reason}");
            } else {
                // The peer refused something we are sending: stop the sender.
                s.cancelled_out.lock().insert(id);
                s.emit(EngineEvent::Notice {
                    level: "warn".into(),
                    message: format!("{} refused the transfer: {reason}", peer.name),
                });
            }
        }
        _ => {}
    }
    Ok(())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn finish(s: &Arc<Shared>, id: u64, mut inc: Incoming) {
    inc.current.take(); // close the last file
    s.stats.transfers_received.fetch_add(1, Relaxed);
    let content = match inc.kind {
        TransferKind::ClipboardText => ClipContent::Text(String::from_utf8_lossy(&inc.data).into_owned()),
        TransferKind::ClipboardImage => ClipContent::Image(std::mem::take(&mut inc.data)),
        TransferKind::ClipboardFiles => ClipContent::Files(inc.top_level.clone()),
        TransferKind::DropFiles => {
            progress(s, id, &inc.from, false, inc.kind, &inc.from, inc.total, inc.total, "done");
            info!(from = %inc.from, files = inc.file_count, "received dropped files");
            s.emit(EngineEvent::Notice {
                level: "info".into(),
                message: format!("Received {} file(s) from {} in {}", inc.file_count, inc.from, s.receive_dir.display()),
            });
            if let Some(first) = inc.top_level.first() {
                s.desktop.reveal(first);
            }
            return;
        }
    };
    if inc.kind == TransferKind::ClipboardFiles {
        progress(s, id, &inc.from, false, inc.kind, &inc.from, inc.total, inc.total, "done");
        s.emit(EngineEvent::Notice {
            level: "info".into(),
            message: format!("{} file(s) from {} are ready: press Ctrl+V in a folder to paste them", inc.file_count, inc.from),
        });
    }
    {
        let mut clip = s.clip.lock();
        clip.last_hash = Some(hash_of(&content));
        clip.pending_files = None;
    }
    let desktop = s.desktop.clone();
    s.rt.spawn_blocking(move || {
        if let Err(e) = desktop.write_clipboard(&content) {
            warn!("could not update the clipboard: {e}");
        }
    });
}

/// Deletes received clipboard files from previous runs.
pub fn clean_temp(temp_dir: &Path) {
    let _ = std::fs::remove_dir_all(temp_dir.join("clipboard"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_paths() {
        assert_eq!(sanitize_rel("a/b.txt"), Some(PathBuf::from("a").join("b.txt")));
        assert_eq!(sanitize_rel("../x"), None);
        assert_eq!(sanitize_rel("a/../../x"), None);
        assert_eq!(sanitize_rel("C:/Windows/x"), Some(PathBuf::from("C_").join("Windows").join("x")));
        assert_eq!(sanitize_rel("con.txt"), Some(PathBuf::from("_con.txt")));
        assert_eq!(sanitize_rel("a?b*c"), Some(PathBuf::from("a_b_c")));
        assert_eq!(sanitize_rel("dots. "), Some(PathBuf::from("dots")));
        assert_eq!(sanitize_rel(""), None);
        assert_eq!(sanitize_rel("\\\\server\\share"), Some(PathBuf::from("server").join("share")));
    }

    #[test]
    fn unique_names() {
        let dir = std::env::temp_dir().join(format!("mkl-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_name(&dir, "a.txt"), "a (1).txt");
        assert_eq!(unique_name(&dir, "b.txt"), "b.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
