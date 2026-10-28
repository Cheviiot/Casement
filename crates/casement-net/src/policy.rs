//! Which source addresses a host accepts by default: loopback and private networks,
//! which covers the LAN and Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48), never the
//! open Internet.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub fn is_private_source(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => private_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => private_v4(v4),
            None => private_v6(v6),
        },
    }
}

fn private_v4(addr: Ipv4Addr) -> bool {
    let [a, b, ..] = addr.octets();
    addr.is_loopback()
        || addr.is_private()
        || addr.is_link_local()
        || (a == 100 && (64..128).contains(&b))
}

fn private_v6(addr: Ipv6Addr) -> bool {
    let first = addr.segments()[0];
    addr.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_tailscale_yes_public_no() {
        for ok in [
            "127.0.0.1",
            "10.1.2.3",
            "172.20.0.1",
            "192.168.0.110",
            "169.254.1.1",
            "100.101.102.103",
            "::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::ffff:192.168.1.2",
        ] {
            assert!(is_private_source(ok.parse().unwrap()), "{ok}");
        }
        for bad in [
            "8.8.8.8",
            "100.128.0.1",
            "172.32.0.1",
            "2001:db8::1",
            "::ffff:1.1.1.1",
        ] {
            assert!(!is_private_source(bad.parse().unwrap()), "{bad}");
        }
    }
}
