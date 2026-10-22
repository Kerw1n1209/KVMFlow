use super::{LanError, Result};
use std::net::{IpAddr, SocketAddr};

/// Explicit allow-list, rather than `!is_global`: CGNAT, benchmark/fake IP,
/// multicast, unspecified and mapped IPv4 addresses must never sneak through.
#[derive(Clone, Copy, Debug, Default)]
pub struct AddressPolicy {
    allow_loopback: bool,
}
impl AddressPolicy {
    pub fn from_env() -> Self {
        Self {
            allow_loopback: std::env::var("KVMFLOW_LAN_ALLOW_LOOPBACK").as_deref() == Ok("1"),
        }
    }
    pub fn allows(self, ip: IpAddr) -> bool {
        if ip.is_loopback() {
            return self.allow_loopback;
        }
        match ip {
            IpAddr::V4(v) => {
                let [a, b, _, _] = v.octets();
                a == 10
                    || (a == 172 && (16..=31).contains(&b))
                    || (a == 192 && b == 168)
                    || (a == 169 && b == 254)
            }
            IpAddr::V6(v) => {
                let s = v.segments();
                s[0] & 0xfe00 == 0xfc00 || s[0] & 0xffc0 == 0xfe80
            }
        }
    }
    pub fn check(self, addr: SocketAddr) -> Result<()> {
        if addr.port() != 0 && self.allows(addr.ip()) {
            Ok(())
        } else {
            Err(LanError::new(
                "E_LAN_ADDRESS",
                format!("address outside LAN allow-list: {addr}"),
            ))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_ranges_and_boundaries() {
        for ip in [
            "10.0.0.0",
            "10.255.255.255",
            "172.16.0.0",
            "172.31.255.255",
            "192.168.0.0",
            "192.168.255.255",
            "169.254.0.0",
            "169.254.255.255",
            "fc00::1",
            "fdff::1",
            "fe80::1",
            "febf::1",
        ] {
            assert!(AddressPolicy::default().allows(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "9.255.255.255",
            "11.0.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.167.1.1",
            "192.169.1.1",
            "169.253.1.1",
            "169.255.1.1",
            "100.64.0.0",
            "100.127.255.255",
            "198.18.0.0",
            "198.19.255.255",
            "127.0.0.1",
            "0.0.0.0",
            "8.8.8.8",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:192.168.1.1",
            "fbff::1",
            "fe00::1",
            "fec0::1",
            "2001:db8::1",
            "ff02::1",
        ] {
            assert!(
                !AddressPolicy::default().allows(ip.parse().unwrap()),
                "{ip}"
            );
        }
        let test_policy = AddressPolicy {
            allow_loopback: true,
        };
        assert!(test_policy.allows("::1".parse().unwrap()));
        assert!(test_policy.allows("127.0.0.1".parse().unwrap()));
        assert!(!test_policy.allows("100.64.0.1".parse().unwrap()));
    }
}
