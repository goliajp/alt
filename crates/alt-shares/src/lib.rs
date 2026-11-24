//! An amount that many take of at once, and what one address may have of it.
//!
//! The bodies being received and the responses being sent are held for as
//! long as their peers take, and a peer may take long. Each kind has an
//! amount for all there are of it, and of that amount the takers of one
//! address have a part between them and no more: one address with many
//! slow connections then leaves the rest to the others.
//!
//! One taker is given more than that: the only one of its address may pass
//! the part, so that what is larger than a part can still be sent and
//! received. It is the one large taker there is then, and has half of the
//! amount at the most: a second that would pass its part is refused until
//! the first is gone, and the other half stays for those that keep within
//! theirs. A single slow peer with one large thing holds half, not all.
//! Where the amount is one that a single taker may pass
//! ([`Shares::passed_by_one_alone`]), a taker that is alone on the server is
//! given whatever it needs: a repository larger than the amount can still be
//! cloned.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

/// the parts the amount has for one address: a quarter each
const PARTS: u64 = 4;

#[derive(Default)]
struct Held {
    all: u64,
    /// what the takers of each address that is counted have between them
    by: HashMap<IpAddr, u64>,
    /// one taker of an address has more than a part
    large: bool,
}

pub struct Shares {
    most: u64,
    /// a taker that is alone has no limit
    alone: bool,
    held: Mutex<Held>,
}

/// What one taker has, given back when it is gone.
pub struct Share {
    of: Arc<Shares>,
    /// the address it counts for; none for this host
    from: Option<IpAddr>,
    bytes: u64,
    /// the one that has more than a part
    large: bool,
}

/// there is no room for what a taker asked for
#[derive(Debug)]
pub struct Full;

impl Shares {
    pub fn of(most: u64) -> Arc<Self> {
        let (alone, held) = (false, Mutex::default());
        Arc::new(Shares { most, alone, held })
    }

    pub fn passed_by_one_alone(most: u64) -> Arc<Self> {
        let (alone, held) = (true, Mutex::default());
        Arc::new(Shares { most, alone, held })
    }

    /// A taker for the address `from`, which has nothing yet.
    pub fn share(self: &Arc<Self>, from: Option<IpAddr>) -> Share {
        let (of, bytes, large) = (Arc::clone(self), 0, false);
        Share {
            of,
            from,
            bytes,
            large,
        }
    }

    /// what all the takers there are have between them
    pub fn held(&self) -> u64 {
        self.held.lock().expect("shares").all
    }
}

/// What a taker for `peer` is counted under: nothing where the peer is this
/// host, the address for IPv4 and on the link, the network of a host for IPv6
/// elsewhere.
pub fn address(peer: IpAddr) -> Option<IpAddr> {
    let peer = match peer {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(peer, IpAddr::V4),
        v4 => v4,
    };
    match peer {
        _ if peer.is_loopback() => None,
        // the hosts of the link the server is on share their first 64 bits
        IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80 => Some(peer),
        IpAddr::V6(v6) => Some(IpAddr::V6((u128::from(v6) >> 64 << 64).into())),
        v4 => Some(v4),
    }
}

/// Half of what is free on the disk `directory` is on.
pub fn half_of_what_is_free(directory: &std::path::Path) -> std::io::Result<u64> {
    use std::os::fd::AsRawFd;
    let directory = std::fs::File::open(directory)?;
    let mut value = std::mem::MaybeUninit::<alt_os::sys::statvfs>::uninit();
    // SAFETY: fstatvfs writes the structure it is pointed to where it succeeds,
    // and the descriptor is open through the call
    if unsafe { alt_os::sys::fstatvfs(directory.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the call succeeded and filled every field
    let value = unsafe { value.assume_init() };
    Ok(((value.f_frsize as u128 * value.f_bavail as u128) / 2).min(u64::MAX as u128) as u64)
}

impl Share {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Takes `bytes` more, or nothing where they are not there to be taken.
    pub fn take(&mut self, bytes: u64) -> Result<(), Full> {
        let mut held = self.of.held.lock().expect("shares");
        let (most, part) = (self.of.most, self.of.most / PARTS);
        let mine = self.bytes + bytes;
        let all = held.all + bytes;
        let alone = self.of.alone && all == mine;
        if all > most && !alone {
            return Err(Full);
        }
        let mut large = self.large;
        if let Some(from) = self.from {
            let theirs = held.by.get(&from).copied().unwrap_or(0) + bytes;
            if theirs > part && theirs > mine {
                return Err(Full);
            }
            large = mine > part;
            if large && !alone && (mine > most / 2 || (held.large && !self.large)) {
                return Err(Full);
            }
            held.by.insert(from, theirs);
        }
        held.large |= large;
        self.large = large;
        held.all = all;
        self.bytes = mine;
        Ok(())
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        let mut held = self.of.held.lock().expect("shares");
        held.all -= self.bytes;
        if self.large {
            held.large = false;
        }
        if let Some(from) = self.from
            && let Some(theirs) = held.by.get_mut(&from)
        {
            *theirs -= self.bytes;
            if *theirs == 0 {
                held.by.remove(&from);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taker(shares: &Arc<Shares>, from: u8, bytes: u64) -> Result<Share, Full> {
        let mut share = shares.share(Some(IpAddr::from([192, 0, 2, from])));
        share.take(bytes)?;
        Ok(share)
    }

    #[test]
    fn the_takers_of_one_address_have_a_quarter_between_them_and_the_rest_is_for_others() {
        let shares = Shares::of(1000);
        // one address fills its quarter with many takers, and has no more
        let many: Vec<Share> = (0..5).map(|_| taker(&shares, 1, 50).unwrap()).collect();
        assert!(taker(&shares, 1, 1).is_err());
        assert_eq!(shares.held(), 250);
        // a taker that had some is refused more, and keeps what it had
        let mut one = shares.share(Some(IpAddr::from([192, 0, 2, 1])));
        assert!(one.take(1).is_err());
        assert_eq!((one.bytes(), shares.held()), (0, 250));
        // the other addresses have theirs, each its quarter
        let others = [2, 3, 4].map(|from| taker(&shares, from, 250).unwrap());
        assert_eq!(shares.held(), 1000);
        // with all of it taken there is none for a fifth address, nor for this host
        assert!(taker(&shares, 5, 1).is_err());
        assert!(shares.share(None).take(1).is_err());
        drop(others);
        // this host is held to the amount alone
        let mut host = shares.share(None);
        host.take(750).unwrap();
        assert!(host.take(1).is_err());
        drop((many, host));
        assert_eq!(shares.held(), 0);
        assert!(shares.held.lock().unwrap().by.is_empty());
    }

    #[test]
    fn one_taker_at_a_time_passes_its_part_and_has_half_and_one_alone_whatever_it_needs() {
        let shares = Shares::of(1000);
        let other = taker(&shares, 2, 100).unwrap();
        // the only taker of its address: past its quarter, as far as half
        let mut only = taker(&shares, 1, 300).unwrap();
        only.take(200).unwrap();
        assert!(only.take(1).is_err());
        // while it has that, a second of its address has nothing
        assert!(taker(&shares, 1, 1).is_err());
        // and no other passes its part, though there is room: within it, it takes
        assert!(taker(&shares, 3, 251).is_err());
        let third = taker(&shares, 3, 250).unwrap();
        // four addresses of one slow taker each do not have it all between them
        let fourth = taker(&shares, 4, 150).unwrap();
        assert_eq!(shares.held(), 1000);
        drop((other, third, fourth));
        // alone on the server it still has half and no more
        assert!(only.take(1).is_err());
        drop(only);
        // the next may pass its part once the one that did is gone
        let next = taker(&shares, 3, 251).unwrap();
        drop(next);
        assert_eq!(shares.held(), 0);
        assert!(!shares.held.lock().unwrap().large);
        // unless the amount is one that a taker alone may pass
        let shares = Shares::passed_by_one_alone(1000);
        let mut only = taker(&shares, 1, 5000).unwrap();
        assert!(taker(&shares, 2, 1).is_err());
        only.take(900).unwrap();
        drop(only);
        let other = taker(&shares, 2, 1).unwrap();
        assert!(taker(&shares, 1, 1000).is_err());
        assert!(taker(&shares, 1, 501).is_err());
        let large = taker(&shares, 1, 500).unwrap();
        drop((other, large));
        assert_eq!(shares.held(), 0);
    }

    #[test]
    fn a_peer_is_counted_under_its_address_or_the_network_of_its_host() {
        let under = |peer: &str| address(peer.parse().unwrap()).map(|under| under.to_string());
        for here in ["127.0.0.1", "::1", "::ffff:127.0.0.9"] {
            assert_eq!(under(here), None, "{here}");
        }
        assert_eq!(under("203.0.113.7").unwrap(), "203.0.113.7");
        assert_eq!(under("::ffff:203.0.113.7").unwrap(), "203.0.113.7");
        assert_eq!(under("2001:db8:1:2:3:4:5:6").unwrap(), "2001:db8:1:2::");
        assert_eq!(under("fe80::1:2").unwrap(), "fe80::1:2");
        let free = half_of_what_is_free(&std::env::temp_dir()).unwrap();
        assert!(free > 0);
    }
}
