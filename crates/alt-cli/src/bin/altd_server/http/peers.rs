//! Which peers are of this host.
//!
//! A connection is counted for the address it comes from, and an address
//! has so many connections and so much of what the server holds. A proxy
//! in front of the server has the connections of everybody: it is of this
//! host, and counted for nothing. On the host itself it comes from a
//! loopback address; before a server in a container it comes from the
//! address the container's network gives the host, which only whoever set
//! the container up knows. So the networks that are of this host are told
//! to the server, the loopback ones where nothing is told.

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
        let address = plain(address.parse().ok()?);
        let most = if address.is_ipv4() { 32 } else { 128 };
        // digits alone: a sign is none
        let digits = |bits: &&str| bits.bytes().all(|byte| byte.is_ascii_digit());
        let bits = match bits {
            Some(bits) => bits.parse().ok().filter(|_| digits(&bits))?,
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

        for told in [
            "",
            "10.0.0.0/33",
            "::/129",
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
}
