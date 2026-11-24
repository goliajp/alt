//! An amount that many take of at once, and what one address may have of it.
//!
//! The bodies being received and the responses being sent are held for as
//! long as their peers take, and a peer may take long. Each kind has an
//! amount for all there are of it, and of that amount the takers of one
//! address have a part between them and no more: one address with many
//! slow connections then leaves the rest to the others.
//!
//! One taker is given more than that: the only one of its address is given
//! what the amount still has, so that what is larger than a part can still
//! be sent and received. Where the amount is one that a single taker may
//! pass ([`Shares::passed_by_one_alone`]), a taker that is alone on the
//! server is given whatever it needs: a repository larger than the amount
//! can still be cloned.

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
}

pub(crate) struct Shares {
    most: u64,
    /// a taker that is alone has no limit
    alone: bool,
    held: Mutex<Held>,
}

/// What one taker has, given back when it is gone.
pub(crate) struct Share {
    of: Arc<Shares>,
    /// the address it counts for; none for this host
    from: Option<IpAddr>,
    bytes: u64,
}

/// there is no room for what a taker asked for
#[derive(Debug)]
pub(crate) struct Full;

impl Shares {
    pub(crate) fn of(most: u64) -> Arc<Self> {
        let (alone, held) = (false, Mutex::default());
        Arc::new(Shares { most, alone, held })
    }

    pub(crate) fn passed_by_one_alone(most: u64) -> Arc<Self> {
        let (alone, held) = (true, Mutex::default());
        Arc::new(Shares { most, alone, held })
    }

    /// A taker for the address `from`, which has nothing yet.
    pub(crate) fn share(self: &Arc<Self>, from: Option<IpAddr>) -> Share {
        let (of, bytes) = (Arc::clone(self), 0);
        Share { of, from, bytes }
    }

    #[cfg(test)]
    pub(crate) fn held(&self) -> u64 {
        self.held.lock().expect("shares").all
    }
}

impl Share {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Takes `bytes` more, or nothing where they are not there to be taken.
    pub(crate) fn take(&mut self, bytes: u64) -> Result<(), Full> {
        let mut held = self.of.held.lock().expect("shares");
        let mine = self.bytes + bytes;
        let all = held.all + bytes;
        if all > self.of.most && !(self.of.alone && all == mine) {
            return Err(Full);
        }
        if let Some(from) = self.from {
            let theirs = held.by.entry(from).or_insert(0);
            if *theirs + bytes > self.of.most / PARTS && *theirs + bytes > mine {
                return Err(Full);
            }
            *theirs += bytes;
        }
        held.all = all;
        self.bytes = mine;
        Ok(())
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        let mut held = self.of.held.lock().expect("shares");
        held.all -= self.bytes;
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
    fn the_only_taker_of_an_address_has_what_is_left_and_one_alone_whatever_it_needs() {
        let shares = Shares::of(1000);
        let other = taker(&shares, 2, 100).unwrap();
        // past its quarter, as far as the amount goes
        let mut only = taker(&shares, 1, 600).unwrap();
        only.take(300).unwrap();
        assert!(only.take(1).is_err());
        // while it has that, a second of its address has nothing
        assert!(taker(&shares, 1, 1).is_err());
        drop(other);
        // alone on the server it has the amount and no more
        only.take(100).unwrap();
        assert!(only.take(1).is_err());
        drop(only);
        assert_eq!(shares.held(), 0);
        // unless the amount is one that a taker alone may pass
        let shares = Shares::passed_by_one_alone(1000);
        let mut only = taker(&shares, 1, 5000).unwrap();
        assert!(taker(&shares, 2, 1).is_err());
        only.take(900).unwrap();
        drop(only);
        let other = taker(&shares, 2, 1).unwrap();
        assert!(taker(&shares, 1, 1000).is_err());
        drop(other);
        assert_eq!(shares.held(), 0);
    }
}
