//! Finding Casement hosts on the LAN with mDNS / DNS-SD.
//!
//! A host announces `_casement._udp.local.` with TXT records `v` (protocol version),
//! `name`, `os`, `os-name` and `fp` (full key fingerprint). Only public facts go into the record;
//! a fingerprint learned here is still confirmed by the TLS handshake.

use std::net::IpAddr;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::auth::sanitize_name;
use crate::identity::Fingerprint;
use crate::{NetError, PROTOCOL_VERSION};

pub const SERVICE_TYPE: &str = "_casement._udp.local.";

fn mdns_error(e: mdns_sd::Error) -> NetError {
    NetError::Io(std::io::Error::other(format!("mDNS: {e}")))
}

/// Announces this host while alive.
pub struct Announcement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Announcement {
    pub fn start(name: &str, port: u16, fingerprint: &Fingerprint) -> Result<Self, NetError> {
        let daemon = ServiceDaemon::new().map_err(mdns_error)?;
        let short = &fingerprint.to_hex()[..8];
        let name = sanitize_name(name);
        let instance = format!(
            "{} ({short})",
            if name.is_empty() { "Casement" } else { &name }
        );
        let version = PROTOCOL_VERSION.to_string();
        let fp = fingerprint.to_hex();
        let os_display_name = sanitize_name(&crate::os_display_name());
        let properties = [
            ("v", version.as_str()),
            ("name", name.as_str()),
            ("os", crate::os_name()),
            ("os-name", os_display_name.as_str()),
            ("fp", fp.as_str()),
        ];
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &format!("casement-{short}.local."),
            "",
            port,
            &properties[..],
        )
        .map_err(mdns_error)?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        daemon.register(info).map_err(mdns_error)?;
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Announcement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    /// mDNS instance name, the key for `Lost`.
    pub id: String,
    pub name: String,
    pub os: String,
    pub os_display_name: String,
    pub fingerprint: Fingerprint,
    /// IPv4 first: they work across more networks than link-local IPv6.
    pub addresses: Vec<IpAddr>,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiscoveryEvent {
    Found(Peer),
    Lost { id: String },
}

pub struct Browser {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
}

impl Browser {
    pub fn start() -> Result<Self, NetError> {
        let daemon = ServiceDaemon::new().map_err(mdns_error)?;
        let events = daemon.browse(SERVICE_TYPE).map_err(mdns_error)?;
        Ok(Self { daemon, events })
    }

    /// The next found or lost host; None when the browser stopped.
    pub async fn next(&self) -> Option<DiscoveryEvent> {
        loop {
            match self.events.recv_async().await.ok()? {
                ServiceEvent::ServiceResolved(info) => {
                    let props = &info.txt_properties;
                    let version = props
                        .get_property_val_str("v")
                        .and_then(|v| v.parse::<u32>().ok());
                    let fingerprint = props
                        .get_property_val_str("fp")
                        .and_then(Fingerprint::from_hex);
                    let (Some(version), Some(fingerprint)) = (version, fingerprint) else {
                        continue;
                    };
                    if version != PROTOCOL_VERSION {
                        continue;
                    }
                    let mut addresses: Vec<IpAddr> =
                        info.addresses.iter().map(|a| a.to_ip_addr()).collect();
                    addresses.sort_by_key(|a| (a.is_ipv6(), *a));
                    let name = sanitize_name(props.get_property_val_str("name").unwrap_or(""));
                    return Some(DiscoveryEvent::Found(Peer {
                        id: info.fullname.clone(),
                        name,
                        os: sanitize_name(props.get_property_val_str("os").unwrap_or("")),
                        os_display_name: sanitize_name(
                            props.get_property_val_str("os-name").unwrap_or(""),
                        ),
                        fingerprint,
                        addresses,
                        port: info.port,
                    }));
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    return Some(DiscoveryEvent::Lost { id: fullname });
                }
                _ => continue,
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}
