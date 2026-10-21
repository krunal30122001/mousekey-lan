use serde::{Deserialize, Serialize};

/// User preferences, persisted as JSON in the `settings` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Name shown to other devices.
    pub device_name: String,
    /// TCP port for incoming connections (takes effect after restart).
    pub listen_port: u16,
    /// Launch when the user signs in.
    pub autostart: bool,
    /// Closing the window keeps the app running in the tray.
    pub close_to_tray: bool,
    /// Re-dial trusted devices automatically.
    pub auto_reconnect: bool,
    /// Advertise and browse via mDNS (takes effect after restart).
    pub discovery_enabled: bool,
    /// Show incoming pairing requests. When off, unknown devices are rejected silently.
    pub accept_pair_requests: bool,
    /// Move to another computer by pushing the cursor against a screen edge.
    pub edge_switching: bool,
    /// Ignore edge hits within `corner_size` pixels of a screen corner.
    pub block_corners: bool,
    pub corner_size: i32,
    /// Require the cursor to rest against the edge this long before switching.
    pub switch_delay_ms: u64,
    /// Ctrl+Alt+Home always returns control to this computer.
    pub return_hotkey: bool,
    /// Physical mouse movement on a controlled computer takes control back.
    pub local_input_reclaims: bool,
    /// Share copied text and images with connected PCs.
    pub share_clipboard: bool,
    /// Share copied files (paste on the other PC) and allow dragging files across edges.
    pub share_files: bool,
    /// Largest file transfer accepted or sent, in MB.
    pub max_transfer_mb: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: default_device_name(),
            listen_port: mkl_protocol::DEFAULT_PORT,
            autostart: true,
            close_to_tray: true,
            auto_reconnect: true,
            discovery_enabled: true,
            accept_pair_requests: true,
            edge_switching: true,
            block_corners: true,
            corner_size: 12,
            switch_delay_ms: 0,
            return_hotkey: true,
            local_input_reclaims: true,
            share_clipboard: true,
            share_files: true,
            max_transfer_mb: 4096,
        }
    }
}

impl Settings {
    /// Clamps values into supported ranges.
    pub fn normalized(mut self) -> Self {
        self.device_name = self.device_name.trim().chars().filter(|c| !c.is_control()).take(48).collect();
        if self.device_name.is_empty() {
            self.device_name = default_device_name();
        }
        if self.listen_port != 0 && self.listen_port < 1024 {
            self.listen_port = mkl_protocol::DEFAULT_PORT;
        }
        self.corner_size = self.corner_size.clamp(0, 200);
        self.switch_delay_ms = self.switch_delay_ms.min(2000);
        self.max_transfer_mb = self.max_transfer_mb.clamp(1, 100_000);
        self
    }
}

pub fn default_device_name() -> String {
    let name = gethostname::gethostname().to_string_lossy().to_string();
    if name.trim().is_empty() {
        "My PC".into()
    } else {
        name
    }
}
