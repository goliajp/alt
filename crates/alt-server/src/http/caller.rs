//! Whom a request came from, where proxies stand in front of the server.
//!
//! The peer of a connection is the last proxy. Each proxy appends the address
//! it took the request from to `X-Forwarded-For`, and leaves standing whatever
//! the sender had written there. So the header is read from its end, and only as
//! far as the addresses are those of proxies the configuration names: the first
//! one that is not is the caller, and nothing before it is looked at. Where the
//! configuration names no proxy the header is not read at all.

use axum::http::HeaderMap;
use std::net::{IpAddr, SocketAddr};

/// An address, or the addresses that share its first `bits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    address: IpAddr,
    bits: u32,
}

/// An IPv4 address written as an IPv6 one is the IPv4 address it is.
fn plain(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4 => v4,
    }
}

impl std::str::FromStr for Network {
    type Err = ();

    /// `192.0.2.7`, `10.0.0.0/8`, `::1`, `fd00::/8`. An IPv4 network
    /// written as an IPv6 one (`::ffff:10.0.0.0/104`) is the IPv4 network
    /// it is; one that is more than the IPv4 addresses is refused.
    fn from_str(text: &str) -> Result<Self, ()> {
        let (address, bits) = match text.split_once('/') {
            Some((address, bits)) => (address, Some(bits)),
            None => (text, None),
        };
        let written: IpAddr = address.parse().map_err(|_| ())?;
        let whole: u32 = if written.is_ipv4() { 32 } else { 128 };
        let bits: u32 = match bits {
            Some(bits) if bits.bytes().all(|b| b.is_ascii_digit()) => {
                bits.parse().map_err(|_| ())?
            }
            Some(_) => return Err(()),
            None => whole,
        };
        if bits > whole {
            return Err(());
        }
        let address = plain(written);
        let bits = match address == written {
            true => bits,
            false => bits.checked_sub(96).ok_or(())?,
        };
        Ok(Network { address, bits })
    }
}

impl Network {
    pub fn contains(&self, address: IpAddr) -> bool {
        // no bits to share is every address of the kind
        let shared = |ours: u128, theirs: u128, whole: u32| {
            let apart = whole.checked_sub(self.bits);
            apart.is_some_and(|apart| (ours ^ theirs).checked_shr(apart).unwrap_or(0) == 0)
        };
        match (self.address, plain(address)) {
            (IpAddr::V4(ours), IpAddr::V4(theirs)) => {
                shared(u32::from(ours).into(), u32::from(theirs).into(), 32)
            }
            (IpAddr::V6(ours), IpAddr::V6(theirs)) => shared(ours.into(), theirs.into(), 128),
            _ => false,
        }
    }
}

/// What an instance with no proxy named says as it starts. It listens on this
/// host alone, so every connection comes from the proxy in front of it.
pub(super) const UNNAMED: &str = "http.trusted_proxies names no proxy: every caller is taken for the peer of its connection, the proxy in front of this instance, and all callers share the sign-in attempts and the request bodies one address may have. Name the proxy, as in \"trusted_proxies\": [\"127.0.0.1\", \"::1\"].";

/// An address as a proxy writes it: bare, in brackets, or with a port.
fn written(text: &str) -> Option<IpAddr> {
    let text = text.trim();
    if let Ok(address) = text.parse::<IpAddr>() {
        return Some(address);
    }
    if let Ok(with_port) = text.parse::<SocketAddr>() {
        return Some(with_port.ip());
    }
    text.strip_prefix('[')?.strip_suffix(']')?.parse().ok()
}

/// The address the request was sent from, as far back as `trusted` proxies vouch
/// for it.
pub(super) fn caller(peer: SocketAddr, headers: &HeaderMap, trusted: &[Network]) -> IpAddr {
    let known = |address: IpAddr| trusted.iter().any(|network| network.contains(address));
    let mut caller = plain(peer.ip());
    let fields = headers.get_all("x-forwarded-for").iter().rev();
    let named = fields.flat_map(|field| field.to_str().unwrap_or("?").rsplit(','));
    for name in named {
        if !known(caller) {
            break;
        }
        // what does not read as an address was written by nobody trusted
        let Some(address) = written(name) else {
            break;
        };
        caller = plain(address);
    }
    caller
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(peer: &str, forwarded: &[&str], trusted: &[&str]) -> String {
        let mut headers = HeaderMap::new();
        for field in forwarded {
            headers.append("x-forwarded-for", field.parse().unwrap());
        }
        let trusted: Vec<Network> = trusted.iter().map(|text| text.parse().unwrap()).collect();
        caller(format!("{peer}:4000").parse().unwrap(), &headers, &trusted).to_string()
    }

    #[test]
    fn the_caller_is_the_first_address_from_the_end_that_is_no_proxy_named() {
        // no proxy is named: the header is whatever the sender wrote
        assert_eq!(from("127.0.0.1", &["203.0.113.7"], &[]), "127.0.0.1");
        assert_eq!(from("198.51.100.1", &["203.0.113.7"], &[]), "198.51.100.1");
        let local = &["127.0.0.1", "::1"];
        assert_eq!(from("127.0.0.1", &["203.0.113.7"], local), "203.0.113.7");
        assert_eq!(from("[::1]", &["203.0.113.7"], local), "203.0.113.7");
        // what the sender wrote comes before what the proxy appended
        assert_eq!(
            from("127.0.0.1", &["192.0.2.9, 203.0.113.7"], local),
            "203.0.113.7"
        );
        assert_eq!(
            from("127.0.0.1", &["192.0.2.9", "203.0.113.7"], local),
            "203.0.113.7"
        );
        // a peer that is no proxy is the caller, whatever it sends
        assert_eq!(
            from("198.51.100.1", &["203.0.113.7"], local),
            "198.51.100.1"
        );
        // two proxies in a row: the one behind is skipped where it is named too
        let both = &["127.0.0.1", "10.0.0.0/8"];
        assert_eq!(
            from("127.0.0.1", &["192.0.2.9, 203.0.113.7, 10.1.2.3"], both),
            "203.0.113.7"
        );
        assert_eq!(
            from("127.0.0.1", &["203.0.113.7, 10.1.2.3"], local),
            "10.1.2.3"
        );
        // every address named is a proxy: the first of them is as far as it goes
        assert_eq!(from("127.0.0.1", &["10.9.9.9, 10.1.2.3"], both), "10.9.9.9");
        // as proxies write them
        for field in [
            "203.0.113.7:5123",
            "[2001:db8::7]:5123",
            "[2001:db8::7]",
            " 2001:db8::7 ",
        ] {
            let expected = if field.contains("2001") {
                "2001:db8::7"
            } else {
                "203.0.113.7"
            };
            assert_eq!(from("127.0.0.1", &[field], local), expected, "{field}");
        }
        assert_eq!(
            from("127.0.0.1", &["::ffff:203.0.113.7"], local),
            "203.0.113.7"
        );
        assert_eq!(
            from("[::ffff:127.0.0.1]", &["203.0.113.7"], local),
            "203.0.113.7"
        );
        // what is no address ends the reading where it stands
        assert_eq!(from("127.0.0.1", &["unknown"], local), "127.0.0.1");
        assert_eq!(
            from("127.0.0.1", &["203.0.113.7, _hidden, 10.1.2.3"], both),
            "10.1.2.3"
        );
    }

    #[test]
    fn a_network_is_an_address_or_the_addresses_that_share_its_first_bits() {
        let of = |text: &str| text.parse::<Network>();
        let has =
            |network: &str, address: &str| of(network).unwrap().contains(address.parse().unwrap());
        assert!(has("10.0.0.0/8", "10.255.0.1") && !has("10.0.0.0/8", "11.0.0.1"));
        assert!(has("192.0.2.7", "192.0.2.7") && !has("192.0.2.7", "192.0.2.8"));
        assert!(has("fd00::/8", "fd12::1") && !has("fd00::/8", "fe80::1"));
        assert!(has("0.0.0.0/0", "203.0.113.7") && !has("0.0.0.0/0", "2001:db8::1"));
        assert!(has("127.0.0.1", "::ffff:127.0.0.1"));
        assert!(has("::/0", "2001:db8::1") && !has("::/0", "203.0.113.7"));
        for bad in [
            "",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/",
            "10.0.0.0/+8",
            "host",
            "10.0.0.0/8/8",
        ] {
            assert!(of(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_ipv4_network_written_as_an_ipv6_one_is_that_network_and_no_more() {
        let of = |text: &str| text.parse::<Network>();
        let has =
            |network: &str, address: &str| of(network).unwrap().contains(address.parse().unwrap());
        for outside in [
            "203.0.113.7",
            "8.8.8.8",
            "::ffff:8.8.8.8",
            "2001:db8::1",
            "::1",
        ] {
            assert!(!has("::ffff:127.0.0.1", outside), "{outside}");
            assert!(!has("::ffff:10.0.0.0/104", outside), "{outside}");
        }
        assert!(has("::ffff:127.0.0.1", "127.0.0.1"));
        assert!(has("::ffff:127.0.0.1", "::ffff:127.0.0.1"));
        assert!(
            has("::ffff:10.0.0.0/104", "10.200.0.1") && !has("::ffff:10.0.0.0/104", "11.0.0.1")
        );
        assert_eq!(of("::ffff:10.0.0.0/104"), of("10.0.0.0/8"));
        // every IPv4 address, and no IPv6 one
        assert!(has("::ffff:0:0/96", "8.8.8.8") && !has("::ffff:0:0/96", "2001:db8::1"));
        // fewer bits than the IPv4 addresses share is no IPv4 network
        for bad in ["::ffff:0:0/95", "::ffff:10.0.0.0/8", "::ffff:127.0.0.1/0"] {
            assert!(of(bad).is_err(), "{bad:?}");
        }
        // a proxy named so vouches for what it appends, and nobody else does
        let named = &["::ffff:127.0.0.1"];
        assert_eq!(from("127.0.0.1", &["203.0.113.7"], named), "203.0.113.7");
        assert_eq!(
            from("198.51.100.1", &["203.0.113.7"], named),
            "198.51.100.1"
        );
        assert_eq!(from("8.8.8.8", &["203.0.113.7"], named), "8.8.8.8");
    }
}
