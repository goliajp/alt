//! Which peers are of this host, and whose request one of them brings.
//!
//! A connection is counted for the address it comes from, and an address
//! has so many connections and so much of what the server holds. A proxy
//! in front of the server has the connections of everybody: it is of this
//! host, and counted for nothing. On the host itself it comes from a
//! loopback address; before a server in a container it comes from the
//! address the container's network gives the host, which only whoever set
//! the container up knows. So the networks that are of this host are told
//! to the server, the loopback ones where nothing is told.
//!
//! A proxy says in `X-Forwarded-For` whose each request is. What a peer of
//! this host says there is taken, as far back as peers of this host vouch
//! for it, and what the request holds is counted for that address; what any
//! other peer says there is not read.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The addresses that begin with so many bits of this one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Network(IpAddr, u8);

/// the loopback addresses
pub(crate) const LOOPBACK: [Network; 2] = [
    Network(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 0)), 8),
    Network(IpAddr::V6(Ipv6Addr::LOCALHOST), 128),
];

/// an IPv4 address written as an IPv6 one is the IPv4 address it is
fn plain(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4 => v4,
    }
}

impl Network {
    fn contains(&self, address: IpAddr) -> bool {
        let bits = |address: IpAddr| match address {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32),
            IpAddr::V6(v6) => (u128::from(v6), 128),
        };
        let ((network, width), (address, of)) = (bits(self.0), bits(address));
        let beyond = width - u32::from(self.1);
        width == of && network.checked_shr(beyond) == address.checked_shr(beyond)
    }
}

/// The networks `told` names: addresses, or addresses with the number of
/// bits that count behind a slash, with commas between them.
pub(crate) fn networks(told: &str) -> Result<Vec<Network>, String> {
    let network = |text: &str| {
        let (address, bits) = match text.split_once('/') {
            Some((address, bits)) => (address, Some(bits)),
            None => (text, None),
        };
        let written: IpAddr = address.parse().ok()?;
        let address = plain(written);
        let most = if address.is_ipv4() { 32 } else { 128 };
        // digits alone: a sign is none
        let digits = |bits: &&str| bits.bytes().all(|byte| byte.is_ascii_digit());
        // the bits of an IPv4 network written as an IPv6 one count from the
        // 96 that say it is one
        let before: u8 = if written.is_ipv6() && address.is_ipv4() {
            96
        } else {
            0
        };
        let bits = match bits {
            Some(bits) => {
                let bits: u8 = bits.parse().ok().filter(|_| digits(&bits))?;
                bits.checked_sub(before)?
            }
            None => most,
        };
        (bits <= most).then_some(Network(address, bits))
    };
    let unread = |text: &str| format!("{text:?} is no address and no network");
    let read = told.split(',').map(str::trim);
    read.map(|text| network(text).ok_or_else(|| unread(text)))
        .collect()
}

/// What the connections of `peer` are counted under: nothing where it is
/// of one of the networks `here`, the address for IPv4 and on the link,
/// the network of a host for IPv6 elsewhere.
pub(crate) fn counted(peer: IpAddr, here: &[Network]) -> Option<IpAddr> {
    let peer = plain(peer);
    match peer {
        _ if here.iter().any(|network| network.contains(peer)) => None,
        // the hosts of the link the server is on share their first 64 bits
        IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80 => Some(peer),
        IpAddr::V6(v6) => Some(IpAddr::V6((u128::from(v6) >> 64 << 64).into())),
        v4 => Some(v4),
    }
}

/// The address `text` names, as proxies write one: bare, with a port, or
/// an IPv6 address in brackets.
fn written(text: &str) -> Option<IpAddr> {
    let text = text.trim();
    if let Ok(address) = text.parse::<IpAddr>() {
        return Some(address);
    }
    if let Ok(with_port) = text.parse::<std::net::SocketAddr>() {
        return Some(with_port.ip());
    }
    text.strip_prefix('[')?.strip_suffix(']')?.parse().ok()
}

/// What a request is counted under that came over a connection counted
/// under `peer` with `forwarded`, the values of its `X-Forwarded-For`
/// headers in the order they came. The last name is the one the nearest
/// proxy wrote; the names are gone through from there for as long as each
/// is of this host, and what does not read as an address was written by
/// nobody that is.
pub(crate) fn asked_from<'a>(
    peer: Option<IpAddr>,
    forwarded: impl DoubleEndedIterator<Item = &'a str>,
    here: &[Network],
) -> Option<IpAddr> {
    if peer.is_some() {
        return peer;
    }
    for name in forwarded.rev().flat_map(|field| field.rsplit(',')) {
        match counted(written(name)?, here) {
            None => continue,
            from => return from,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counted_under(peer: &str, here: &[Network]) -> Option<String> {
        counted(peer.parse().unwrap(), here).map(|address| address.to_string())
    }

    #[test]
    fn a_peer_of_a_network_that_is_told_to_be_of_this_host_is_counted_for_nothing() {
        // nothing told: the loopback addresses, however they are written
        for peer in ["127.0.0.1", "127.8.9.10", "::1", "::ffff:127.0.0.1"] {
            assert_eq!(counted_under(peer, &LOOPBACK), None, "{peer}");
        }
        let under = |peer: &str| counted_under(peer, &LOOPBACK).unwrap();
        assert_eq!(under("172.30.90.1"), "172.30.90.1");
        assert_eq!(under("::ffff:192.0.2.7"), "192.0.2.7");
        assert_eq!(under("2001:db8:1:2:3:4:5:6"), "2001:db8:1:2::");
        assert_eq!(under("fe80::1:2"), "fe80::1:2");

        // the gateway of a container's network, a network, and one of IPv6
        let here = networks("172.30.90.1, 10.1.0.0/16,2001:db8:5::/48").unwrap();
        for peer in ["172.30.90.1", "::ffff:172.30.90.1", "10.1.255.4"] {
            assert_eq!(counted_under(peer, &here), None, "{peer}");
        }
        assert_eq!(counted_under("2001:db8:5:ffff::9", &here), None);
        for peer in ["172.30.90.2", "10.2.0.1", "2001:db8:6::1", "127.0.0.1"] {
            assert!(counted_under(peer, &here).is_some(), "{peer}");
        }
        // every address, and none
        let every = networks("0.0.0.0/0,::/0").unwrap();
        assert_eq!(counted_under("198.51.100.4", &every), None);
        assert_eq!(counted_under("2001:db8::4", &every), None);
        assert_eq!(
            networks("::ffff:10.0.0.1").unwrap(),
            networks("10.0.0.1").unwrap()
        );
        assert_eq!(
            networks("::ffff:10.0.0.0/104").unwrap(),
            networks("10.0.0.0/8").unwrap()
        );

        for told in [
            "",
            "10.0.0.0/33",
            "::/129",
            "::ffff:10.0.0.0/95",
            "::ffff:10.0.0.0/129",
            "host",
            "10.0.0.1/",
            "1.2.3.4/+8",
            "1.2.3.4,",
        ] {
            let refused = networks(told).unwrap_err();
            assert!(
                refused.contains("is no address and no network"),
                "{told}: {refused}"
            );
        }
    }

    #[test]
    fn a_request_a_peer_of_this_host_brings_is_counted_for_whom_it_says_it_is_of() {
        let here = networks("127.0.0.0/8,::1,172.30.90.1").unwrap();
        let from = |peer: &str, forwarded: &[&str]| {
            let peer = counted(peer.parse().unwrap(), &here);
            let asked = asked_from(peer, forwarded.iter().copied(), &here);
            asked.map(|address| address.to_string())
        };
        let some = |text: &str| Some(text.to_owned());
        // what a peer that is not of this host says is not read
        assert_eq!(from("203.0.113.7", &["198.51.100.1"]), some("203.0.113.7"));
        // the proxy says who, as it writes addresses
        assert_eq!(from("127.0.0.1", &["198.51.100.1"]), some("198.51.100.1"));
        assert_eq!(
            from("172.30.90.1", &["198.51.100.1:4711"]),
            some("198.51.100.1")
        );
        assert_eq!(from("::1", &["[2001:db8:1:2::9]"]), some("2001:db8:1:2::"));
        // the last name is the nearest proxy's: what stands before it the client sent
        assert_eq!(
            from("127.0.0.1", &["10.9.9.9, 198.51.100.1"]),
            some("198.51.100.1")
        );
        assert_eq!(
            from("127.0.0.1", &["10.9.9.9", "198.51.100.2"]),
            some("198.51.100.2")
        );
        // through proxies of this host, as far back as they go
        assert_eq!(
            from("127.0.0.1", &["198.51.100.3, 172.30.90.1, 127.0.0.1"]),
            some("198.51.100.3")
        );
        // nothing said, nothing that reads, or this host itself: counted for nothing
        assert_eq!(from("127.0.0.1", &[]), None);
        assert_eq!(from("127.0.0.1", &["198.51.100.1, unknown"]), None);
        assert_eq!(from("127.0.0.1", &["127.0.0.1"]), None);
    }
}
