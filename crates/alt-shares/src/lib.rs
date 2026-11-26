//! An amount that many take of at once, and what one address may have of it.
//!
//! The bodies being received and the responses being sent are held for as
//! long as their peers take, and a peer may take long. Each kind has an
//! amount for all there are of it, and of that amount the takers of one
//! address have a part between them and no more: one address with many
//! slow connections then leaves the rest to the others.
//!
//! A taker may pass the part, so that what is larger than a part can still
//! be sent and received: it is then a large one, and has half of the amount
//! at the most. One address has one large taker at a time, and the server as
//! many as it is told ([`Shares::of`]), half its workers. A taker that would
//! be large while that many are, or while its address has one, waits for its
//! turn where it is told how long it may ([`Share::waiting`]), and is
//! refused otherwise. The turns go round the addresses that wait: the many
//! takers of one address do not keep those of another waiting.
//! Where the amount is one that a single taker may pass
//! ([`Shares::passed_by_one_alone`]), a taker that is alone on the server is
//! given whatever it needs: a repository larger than the amount can still be
//! cloned.
//!
//! A large taker that one of another address waits for has to go on faster
//! than a peer has to in general: what it holds is sent at no less than
//! [`LARGE_RATE`] in each [`LARGE_WINDOW`] while that one waits
//! ([`Share::sent`]). A peer on a slow line that is alone in a turn has it for
//! as long as it takes, and so has one whose own address alone waits after
//! it; one that would keep a turn by taking a kilobyte a second, the least a
//! peer takes before it is let go of, is let go of once another address wants
//! the turn.

mod disk;
mod held;
mod share;

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

pub use disk::half_of_what_is_free;
use held::Held;
pub use share::{Aside, Building, Full, Share, Slow};

/// the parts the amount has for one address: a quarter each
const PARTS: u64 = 4;
/// how often a taker that waits for its turn looks whether it is still waited for
const LOOK: Duration = Duration::from_millis(100);
/// what a large taker that another waits for sends at the least, in bytes a second
pub const LARGE_RATE: u64 = 1 << 20;
/// the time over which that is counted
pub const LARGE_WINDOW: Duration = Duration::from_secs(30);

pub struct Shares {
    pub(crate) most: u64,
    /// a taker that is alone has no limit
    pub(crate) alone: bool,
    /// the large takers there may be at once
    pub(crate) large: usize,
    held: Mutex<Held>,
    /// told whenever a large taker is gone or one that waits is let in
    pub(crate) changed: Condvar,
    /// the takers being built of what they are answered from
    pub(crate) building: AtomicUsize,
}

impl Shares {
    /// `most` bytes, of which `large` takers at once may have more than a part.
    pub fn of(most: u64, large: usize) -> Arc<Self> {
        Self::new(most, large, false)
    }

    pub fn passed_by_one_alone(most: u64, large: usize) -> Arc<Self> {
        Self::new(most, large, true)
    }

    fn new(most: u64, large: usize, alone: bool) -> Arc<Self> {
        let pace = (LARGE_RATE, LARGE_WINDOW);
        let held = Mutex::new(Held {
            pace,
            ..Held::default()
        });
        let (changed, large) = (Condvar::new(), large.max(1));
        let building = AtomicUsize::new(0);
        Arc::new(Shares {
            most,
            alone,
            large,
            held,
            changed,
            building,
        })
    }

    /// The large takers another waits for send `rate` bytes a second in each
    /// `window` at the least, and not [`LARGE_RATE`] in [`LARGE_WINDOW`].
    pub fn paced(self: Arc<Self>, rate: u64, window: Duration) -> Arc<Self> {
        self.lock().pace = (rate, window);
        self
    }

    /// what a large taker another waits for sends at the least, in bytes a
    /// second, and the time over which that is counted
    pub fn pace(&self) -> (u64, Duration) {
        self.lock().pace
    }

    /// A taker for the address `from`, which has nothing yet.
    pub fn share(self: &Arc<Self>, from: Option<IpAddr>) -> Share {
        Share::new(Arc::clone(self), from)
    }

    /// the amount there is for all the takers
    pub fn most(&self) -> u64 {
        self.most
    }

    /// what all the takers there are have between them
    pub fn held(&self) -> u64 {
        self.lock().all
    }

    /// the takers being built now (see [`Share::building`])
    pub fn building(&self) -> usize {
        self.building.load(Ordering::Relaxed)
    }

    /// the takers that wait for their turn to be large
    pub fn waiting(&self) -> usize {
        self.lock().waiting.values().map(VecDeque::len).sum()
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().expect("shares")
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

#[cfg(test)]
mod tests;
