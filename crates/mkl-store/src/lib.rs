//! SQLite persistence: the device's own identity key, paired / blocked devices, the monitor
//! layout and user settings. No input data or credentials are ever stored.

mod protect;

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use mkl_protocol::Rect;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("key protection error: {0}")]
    Protect(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    pub device_id: String,
    pub name: String,
    pub public_key: Vec<u8>,
    pub trusted: bool,
    pub blocked: bool,
    pub last_addr: Option<String>,
    pub last_seen: Option<i64>,
    pub paired_at: Option<i64>,
    pub screens: Vec<Rect>,
}

const SCHEMA_VERSION: i32 = 1;
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS identity (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    private_key BLOB NOT NULL,
    public_key BLOB NOT NULL,
    protected INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices (
    device_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    public_key BLOB NOT NULL,
    trusted INTEGER NOT NULL DEFAULT 0,
    blocked INTEGER NOT NULL DEFAULT 0,
    last_addr TEXT,
    last_seen INTEGER,
    paired_at INTEGER,
    screens TEXT NOT NULL DEFAULT '[]'
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS layout (
    device_id TEXT PRIMARY KEY,
    x INTEGER NOT NULL,
    y INTEGER NOT NULL
);
";

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Returns `(private, public)`, generating and persisting a new identity on first run.
    /// The private key is protected with Windows DPAPI (bound to the current user).
    pub fn load_or_create_identity(
        &self,
        generate: impl FnOnce() -> (Vec<u8>, Vec<u8>),
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let conn = self.conn();
        let row: Option<(Vec<u8>, Vec<u8>, bool)> = conn
            .query_row(
                "SELECT private_key, public_key, protected FROM identity WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((private, public, protected)) = row {
            let private = if protected { protect::unprotect(&private)? } else { private };
            return Ok((private, public));
        }
        let (private, public) = generate();
        let (stored, protected) = match protect::protect(&private) {
            Ok(p) => (p, true),
            Err(e) => {
                tracing::warn!("storing identity key without OS protection: {e}");
                (private.clone(), false)
            }
        };
        conn.execute(
            "INSERT INTO identity (id, private_key, public_key, protected, created_at) VALUES (1, ?1, ?2, ?3, ?4)",
            params![stored, public, protected, now_unix()],
        )?;
        Ok((private, public))
    }

    pub fn get_device(&self, id: &str) -> Result<Option<DeviceRecord>> {
        let conn = self.conn();
        Ok(conn
            .query_row(&format!("{SELECT_DEVICE} WHERE device_id = ?1"), [id], row_to_device)
            .optional()?)
    }

    pub fn list_devices(&self) -> Result<Vec<DeviceRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("{SELECT_DEVICE} ORDER BY name COLLATE NOCASE"))?;
        let rows = stmt.query_map([], row_to_device)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Inserts or fully updates a device row (keeps the original `paired_at`).
    pub fn save_device(&self, d: &DeviceRecord) -> Result<()> {
        let screens = serde_json::to_string(&d.screens)?;
        self.conn().execute(
            "INSERT INTO devices (device_id, name, public_key, trusted, blocked, last_addr, last_seen, paired_at, screens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(device_id) DO UPDATE SET
                name = excluded.name, public_key = excluded.public_key, trusted = excluded.trusted,
                blocked = excluded.blocked, last_addr = COALESCE(excluded.last_addr, devices.last_addr),
                last_seen = COALESCE(excluded.last_seen, devices.last_seen),
                paired_at = COALESCE(devices.paired_at, excluded.paired_at), screens = excluded.screens",
            params![
                d.device_id, d.name, d.public_key, d.trusted, d.blocked, d.last_addr, d.last_seen, d.paired_at, screens
            ],
        )?;
        Ok(())
    }

    pub fn set_trusted(&self, id: &str, trusted: bool) -> Result<()> {
        self.conn()
            .execute("UPDATE devices SET trusted = ?2 WHERE device_id = ?1", params![id, trusted])?;
        Ok(())
    }

    /// Blocks a device (creating a row if needed). Blocking also revokes trust.
    pub fn block_device(&self, id: &str, name: &str, public_key: &[u8]) -> Result<()> {
        self.conn().execute(
            "INSERT INTO devices (device_id, name, public_key, trusted, blocked) VALUES (?1, ?2, ?3, 0, 1)
             ON CONFLICT(device_id) DO UPDATE SET trusted = 0, blocked = 1",
            params![id, name, public_key],
        )?;
        Ok(())
    }

    /// Unblocking forgets the device entirely; it must pair again.
    pub fn unblock_device(&self, id: &str) -> Result<()> {
        self.delete_device(id)
    }

    pub fn delete_device(&self, id: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM devices WHERE device_id = ?1", [id])?;
        conn.execute("DELETE FROM layout WHERE device_id = ?1", [id])?;
        Ok(())
    }

    pub fn touch_device(&self, id: &str, addr: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE devices SET last_seen = ?2, last_addr = COALESCE(?3, last_addr) WHERE device_id = ?1",
            params![id, now_unix(), addr],
        )?;
        Ok(())
    }

    pub fn update_screens(&self, id: &str, screens: &[Rect]) -> Result<()> {
        let json = serde_json::to_string(screens)?;
        self.conn()
            .execute("UPDATE devices SET screens = ?2 WHERE device_id = ?1", params![id, json])?;
        Ok(())
    }

    pub fn get_setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> = self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        match raw {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)?;
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, json],
        )?;
        Ok(())
    }

    pub fn load_layout(&self) -> Result<Vec<(String, i32, i32)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT device_id, x, y FROM layout")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn save_layout(&self, positions: &[(String, i32, i32)]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM layout", [])?;
        {
            let mut stmt = tx.prepare("INSERT INTO layout (device_id, x, y) VALUES (?1, ?2, ?3)")?;
            for (id, x, y) in positions {
                stmt.execute(params![id, x, y])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

const SELECT_DEVICE: &str =
    "SELECT device_id, name, public_key, trusted, blocked, last_addr, last_seen, paired_at, screens FROM devices";

fn row_to_device(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRecord> {
    let screens: String = r.get(8)?;
    Ok(DeviceRecord {
        device_id: r.get(0)?,
        name: r.get(1)?,
        public_key: r.get(2)?,
        trusted: r.get(3)?,
        blocked: r.get(4)?,
        last_addr: r.get(5)?,
        last_seen: r.get(6)?,
        paired_at: r.get(7)?,
        screens: serde_json::from_str(&screens).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str) -> DeviceRecord {
        DeviceRecord {
            device_id: id.into(),
            name: format!("PC {id}"),
            public_key: vec![1, 2, 3],
            trusted: true,
            blocked: false,
            last_addr: Some("10.0.0.2:47820".into()),
            last_seen: Some(5),
            paired_at: Some(5),
            screens: vec![Rect::new(0, 0, 1920, 1080)],
        }
    }

    #[test]
    fn identity_is_created_once() {
        let s = Store::open_in_memory().unwrap();
        let a = s.load_or_create_identity(|| (vec![7; 32], vec![8; 32])).unwrap();
        let b = s.load_or_create_identity(|| panic!("must not regenerate")).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.0, vec![7; 32]);
    }

    #[test]
    fn device_lifecycle() {
        let s = Store::open_in_memory().unwrap();
        s.save_device(&device("a")).unwrap();
        assert_eq!(s.get_device("a").unwrap().unwrap().screens.len(), 1);
        s.set_trusted("a", false).unwrap();
        assert!(!s.get_device("a").unwrap().unwrap().trusted);
        s.block_device("a", "x", &[1]).unwrap();
        let d = s.get_device("a").unwrap().unwrap();
        assert!(d.blocked && !d.trusted);
        s.block_device("b", "B", &[9]).unwrap();
        assert_eq!(s.list_devices().unwrap().len(), 2);
        s.unblock_device("b").unwrap();
        assert!(s.get_device("b").unwrap().is_none());
    }

    #[test]
    fn settings_and_layout() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.get_setting::<u32>("x").unwrap(), None);
        s.set_setting("x", &42u32).unwrap();
        assert_eq!(s.get_setting::<u32>("x").unwrap(), Some(42));
        s.save_layout(&[("a".into(), 0, 0), ("b".into(), 1920, 0)]).unwrap();
        assert_eq!(s.load_layout().unwrap().len(), 2);
        s.delete_device("b").unwrap();
        assert_eq!(s.load_layout().unwrap().len(), 1);
    }
}
