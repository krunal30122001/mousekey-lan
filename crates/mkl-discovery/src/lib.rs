//! LAN discovery over mDNS / DNS-SD (`_mousekeylan._tcp.local.`).
//!
//! Discovery only advertises *where* a device listens. It carries no trust: every connection
//! is still authenticated by the Noise handshake and must be paired explicitly.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use mkl_protocol::SERVICE_TYPE;

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("mDNS error: {0}")]
    Mdns(#[from] mdns_sd::Error),
    #[error("failed to start discovery thread: {0}")]
    Thread(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPeer {
    pub device_id: String,
    pub name: String,
    /// Reachable addresses, IPv4 first.
    pub addrs: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    Found(DiscoveredPeer),
    Lost(String),
}

/// Advertises this device and browses for others until dropped.
pub struct Discovery {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Discovery {
    pub fn start(
        device_id: &str,
        name: &str,
        port: u16,
        on_event: impl Fn(DiscoveryEvent) + Send + 'static,
    ) -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        let host = format!("mkl-{device_id}.local.");
        let mut props = HashMap::new();
        props.insert("id".to_string(), device_id.to_string());
        props.insert("name".to_string(), name.chars().take(63).collect::<String>());
        props.insert("v".to_string(), mkl_protocol::PROTOCOL_VERSION.to_string());
        let info = ServiceInfo::new(SERVICE_TYPE, device_id, &host, "", port, props)?.enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        daemon.register(info)?;

        let receiver = daemon.browse(SERVICE_TYPE)?;
        let my_id = device_id.to_string();
        std::thread::Builder::new().name("mkl-mdns".into()).spawn(move || {
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let Some(id) = info.get_property_val_str("id").map(str::to_string) else { continue };
                        if id == my_id || id.len() != 16 {
                            continue;
                        }
                        let name = info.get_property_val_str("name").unwrap_or(&id).to_string();
                        let port = info.get_port();
                        let mut addrs: Vec<SocketAddr> = info
                            .get_addresses()
                            .iter()
                            .filter_map(|a| a.to_string().parse::<IpAddr>().ok())
                            // IPv6 link-local addresses need a scope id that mDNS does not carry.
                            .filter(|ip| !ip.is_loopback() && !ip.is_unspecified() && !is_link_local_v6(ip))
                            .map(|ip| SocketAddr::new(ip, port))
                            .collect();
                        addrs.sort_by_key(|a| (!a.is_ipv4(), a.to_string()));
                        if addrs.is_empty() {
                            continue;
                        }
                        tracing::debug!(%id, %name, ?addrs, "discovered device");
                        on_event(DiscoveryEvent::Found(DiscoveredPeer { device_id: id, name, addrs }));
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        if let Some(id) = fullname.split('.').next() {
                            if id != my_id {
                                on_event(DiscoveryEvent::Lost(id.to_string()));
                            }
                        }
                    }
                    ServiceEvent::SearchStopped(_) => break,
                    _ => {}
                }
            }
        })?;
        tracing::info!(service = SERVICE_TYPE, port, "mDNS discovery started");
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

fn is_link_local_v6(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_local_detection() {
        assert!(is_link_local_v6(&"fe80::ddbf:e87a:de80:d1ec".parse().unwrap()));
        assert!(!is_link_local_v6(&"192.168.1.8".parse().unwrap()));
        assert!(!is_link_local_v6(&"2001:db8::1".parse().unwrap()));
    }
}
