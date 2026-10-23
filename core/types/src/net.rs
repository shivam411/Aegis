//! IP address ranges ("CIDRs") for the proxy's `trusted_proxies` list.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

/// An IP address or a network in CIDR notation (`127.0.0.1`, `10.0.0.0/8`,
/// `::1`, `fd00::/8`). A bare address is a /32 (IPv4) or /128 (IPv6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IpCidr {
    network: IpAddr,
    prefix: u8,
}

impl IpCidr {
    /// The prefix length in bits.
    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// The network address (host bits cleared).
    pub fn network(&self) -> IpAddr {
        self.network
    }

    /// Whether every address in the range is a loopback address.
    pub fn is_loopback(&self) -> bool {
        match self.network {
            IpAddr::V4(v4) => v4.octets()[0] == 127 && self.prefix >= 8,
            IpAddr::V6(v6) => v6.is_loopback() && self.prefix == 128,
        }
    }

    /// Whether `ip` is in the range. IPv4-mapped IPv6 addresses
    /// (`::ffff:127.0.0.1`, as a dual-stack listener reports IPv4 peers)
    /// are compared as the IPv4 address they carry.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = mask32(self.prefix);
                u32::from(ip) & mask == u32::from(net)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = mask128(self.prefix);
                u128::from(ip) & mask == u128::from(net)
            }
            _ => false,
        }
    }
}

fn mask32(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    }
}

fn mask128(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(prefix))
    }
}

impl FromStr for IpCidr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (addr, prefix) = match s.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (s, None),
        };
        let ip: IpAddr = addr.parse().map_err(|_| {
            format!(
                "\"{}\" is not an IP address or CIDR range (such as \"127.0.0.1\" or \"10.0.0.0/8\"); \
                 host names are not accepted",
                s
            )
        })?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) => match p.parse::<u8>() {
                Ok(n) if n <= max && p.bytes().all(|b| b.is_ascii_digit()) => n,
                _ => {
                    return Err(format!(
                        "\"{}\" has an invalid prefix length (must be 0-{})",
                        s, max
                    ))
                }
            },
        };
        let network = match ip {
            IpAddr::V4(v4) => IpAddr::V4((u32::from(v4) & mask32(prefix)).into()),
            IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & mask128(prefix)).into()),
        };
        Ok(Self { network, prefix })
    }
}

impl fmt::Display for IpCidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(s: &str) -> IpCidr {
        s.parse().unwrap()
    }

    #[test]
    fn parses_addresses_and_ranges() {
        assert_eq!(cidr("127.0.0.1").prefix(), 32);
        assert_eq!(cidr("::1").prefix(), 128);
        assert_eq!(cidr("10.1.2.3/8").to_string(), "10.0.0.0/8");
        assert_eq!(cidr(" fd00::1/8 ").to_string(), "fd00::/8");
        for bad in [
            "",
            "localhost",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/",
            "10.0.0.0/x",
            "10.0.0.0/+8",
            "1.2.3",
            "10.0.0.0/8/8",
        ] {
            assert!(bad.parse::<IpCidr>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn matches_addresses() {
        let local = cidr("127.0.0.1");
        assert!(local.contains("127.0.0.1".parse().unwrap()));
        assert!(local.contains("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!local.contains("127.0.0.2".parse().unwrap()));
        assert!(!local.contains("::1".parse().unwrap()));
        let net = cidr("10.0.0.0/8");
        assert!(net.contains("10.200.0.1".parse().unwrap()));
        assert!(!net.contains("11.0.0.1".parse().unwrap()));
        let v6 = cidr("fd00::/8");
        assert!(v6.contains("fd12::5".parse().unwrap()));
        assert!(!v6.contains("fe80::1".parse().unwrap()));
        assert!(cidr("0.0.0.0/0").contains("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn loopback_ranges() {
        assert!(cidr("127.0.0.1").is_loopback());
        assert!(cidr("127.0.0.0/8").is_loopback());
        assert!(cidr("::1").is_loopback());
        assert!(!cidr("0.0.0.0/0").is_loopback());
        assert!(!cidr("10.0.0.1").is_loopback());
        assert!(!cidr("::/0").is_loopback());
    }
}
