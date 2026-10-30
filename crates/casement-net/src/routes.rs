//! Multiple routes to one pinned device. No password is sent until a route wins.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use crate::client::{self, ClientInfo, Handshake};
use crate::{Fingerprint, Identity, NetError};

const MAX_ROUTES: usize = 16;

pub fn is_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]),
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

fn usable(ip: IpAddr) -> bool {
    !ip.is_unspecified()
        && !ip.is_multicast()
        && match ip {
            IpAddr::V4(ip) => !ip.is_link_local() && !ip.is_broadcast(),
            IpAddr::V6(ip) => !ip.is_unicast_link_local(),
        }
}

/// Only advertise interfaces on which this host is actually listening.
pub(crate) fn local_endpoints(local: Option<IpAddr>, bind: SocketAddr) -> Vec<String> {
    if !bind.ip().is_unspecified() {
        return vec![bind.to_string()];
    }
    let mut ips: Vec<_> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| interface.ip())
        .filter(|ip| usable(*ip) && !ip.is_loopback())
        .collect();
    ips.extend(local.filter(|ip| usable(*ip) && !ip.is_loopback()));
    // Retain a tailnet route even on hosts with many container/VM interfaces.
    ips.sort_by_key(|ip| (!is_tailnet(*ip), ip.is_ipv6(), *ip));
    ips.dedup();
    ips.truncate(MAX_ROUTES);
    ips.sort_by_key(|ip| (is_tailnet(*ip), ip.is_ipv6(), *ip));
    ips.into_iter()
        .take(MAX_ROUTES)
        .map(|ip| SocketAddr::new(ip, bind.port()).to_string())
        .collect()
}

/// Start LAN immediately, then overlap alternatives instead of waiting ten seconds
/// on a stale home address. Every attempt uses exactly the same expected key.
pub async fn connect(
    identity: Arc<Identity>,
    targets: Vec<(String, u16)>,
    expected: Option<Fingerprint>,
    info: ClientInfo,
    device_name: String,
) -> Result<(Handshake, SocketAddr), NetError> {
    let mut attempts = tokio::task::JoinSet::new();
    spawn_targets(&mut attempts, &identity, targets.clone(), expected, &info);
    // Bootstrap old LAN-only records from the local Tailscale daemon. A name is
    // only a ranking hint: TLS must still prove the already saved device key.
    if expected.is_some() && !device_name.is_empty() {
        attempts.spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let port = targets
                .first()
                .map(|(_, port)| *port)
                .unwrap_or(crate::DEFAULT_PORT);
            let targets = tailscale_targets(&device_name, port).await;
            let mut alternatives = tokio::task::JoinSet::new();
            spawn_targets(&mut alternatives, &identity, targets, expected, &info);
            first_match(alternatives).await
        });
    }
    first_match(attempts).await
}

type Attempts = tokio::task::JoinSet<Result<(Handshake, SocketAddr), NetError>>;

fn spawn_targets(
    attempts: &mut Attempts,
    identity: &Arc<Identity>,
    mut targets: Vec<(String, u16)>,
    expected: Option<Fingerprint>,
    info: &ClientInfo,
) {
    targets.sort_by_key(|(host, _)| {
        host.parse::<IpAddr>().is_ok_and(is_tailnet) || host.ends_with(".ts.net")
    });
    let mut seen = std::collections::HashSet::new();
    for (index, (host, port)) in targets
        .into_iter()
        .filter(|target| seen.insert(target.clone()))
        .take(MAX_ROUTES)
        .enumerate()
    {
        let identity = identity.clone();
        let info = info.clone();
        attempts.spawn(async move {
            tokio::time::sleep(Duration::from_millis(index as u64 * 75)).await;
            tokio::time::timeout(Duration::from_secs(12), async move {
                let addresses = tokio::net::lookup_host((host.as_str(), port)).await?;
                let mut routes = tokio::task::JoinSet::new();
                for addr in addresses.filter(|addr| usable(addr.ip())).take(4) {
                    let identity = identity.clone();
                    let info = info.clone();
                    routes.spawn(async move {
                        let handshake = client::connect(&identity, addr, expected, &info).await?;
                        Ok((handshake, addr))
                    });
                }
                first_match(routes).await
            })
            .await
            .map_err(|_| NetError::Timeout)?
        });
    }
}

async fn first_match(mut attempts: Attempts) -> Result<(Handshake, SocketAddr), NetError> {
    let mut failure = NetError::Timeout;
    while let Some(result) = attempts.join_next().await {
        match result {
            Ok(Ok(connected)) => return Ok(connected),
            Ok(Err(error)) => {
                // A bad candidate must not hide a reachable route to the right OS.
                // If none match, retain the key warning instead of a later timeout.
                if !matches!(failure, NetError::KeyChanged { .. }) {
                    failure = error;
                }
            }
            Err(_) => {}
        }
    }
    Err(failure)
}

async fn tailscale_targets(name: &str, port: u16) -> Vec<(String, u16)> {
    #[cfg(target_os = "windows")]
    let executable = std::env::var_os("ProgramFiles")
        .map(|base| std::path::PathBuf::from(base).join("Tailscale/tailscale.exe"))
        .unwrap_or_else(|| "tailscale.exe".into());
    #[cfg(not(target_os = "windows"))]
    let executable = "tailscale";
    let mut command = tokio::process::Command::new(executable);
    command.args(["status", "--json"]).kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(2), command.output()).await
    else {
        return Vec::new();
    };
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        return Vec::new();
    }
    parse_tailnet(&output.stdout, name, port)
}

fn parse_tailnet(json: &[u8], name: &str, port: u16) -> Vec<(String, u16)> {
    let Ok(status) = serde_json::from_slice::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(peers) = status.get("Peer").and_then(|peers| peers.as_object()) else {
        return Vec::new();
    };
    let mut peers: Vec<_> = peers
        .values()
        .filter(|peer| peer.get("Online").and_then(|value| value.as_bool()) == Some(true))
        .collect();
    let name = name.to_lowercase();
    peers.sort_by_key(|peer| {
        ["HostName", "DNSName"]
            .iter()
            .map(|key| {
                let host = peer
                    .get(key)
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                if host == name {
                    0
                } else if host.starts_with(&format!("{name}-")) {
                    1
                } else {
                    2
                }
            })
            .min()
            .unwrap_or(2)
    });
    peers
        .into_iter()
        .filter_map(|peer| peer.get("TailscaleIPs").and_then(|ips| ips.as_array()))
        .flatten()
        .filter_map(|ip| ip.as_str()?.parse::<IpAddr>().ok())
        .filter(|ip| is_tailnet(*ip))
        .take(MAX_ROUTES)
        .map(|ip| (ip.to_string(), port))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailnet_bootstrap_matches_names_but_only_uses_tailnet_addresses() {
        let json = br#"{"Peer":{"a":{"HostName":"gem12-alt","Online":true,"DNSName":"gem12-alt.example.ts.net.","TailscaleIPs":["100.123.140.55","192.168.0.110","bad"]},"b":{"HostName":"other","Online":false,"TailscaleIPs":["100.64.0.3"]}}}"#;
        assert_eq!(
            parse_tailnet(json, "GEM12", 47820),
            [("100.123.140.55".into(), 47820)]
        );
        assert_eq!(parse_tailnet(json, "gem12-alt", 47820).len(), 1);
        // A user-defined card name need not equal the OS-specific Tailscale name.
        assert_eq!(parse_tailnet(json, "Дом", 47820).len(), 1);
        assert!(parse_tailnet(b"broken", "gem12", 47820).is_empty());
    }
}
