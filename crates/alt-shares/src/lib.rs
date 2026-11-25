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
//! A large taker that another waits for has to go on faster than a peer has
//! to in general: what it holds is sent at no less than [`LARGE_RATE`] in each
//! [`LARGE_WINDOW`] while another waits ([`Share::sent`]). A peer on a slow
//! line that is alone in a turn has it for as long as it takes; one that would
//! keep a turn by taking a kilobyte a second, the least a peer takes before
//! it is let go of, is let go of once another wants the turn.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// the parts the amount has for one address: a quarter each
const PARTS: u64 = 4;
/// how often a taker that waits for its turn looks whether it is still waited for
const LOOK: Duration = Duration::from_millis(100);
/// what a large taker that another waits for sends at the least, in bytes a second
pub const LARGE_RATE: u64 = 1 << 20;
/// the time over which that is counted
pub const LARGE_WINDOW: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Held {
    all: u64,
    /// what the takers of each address that keep within the part have between them
    by: HashMap<IpAddr, u64>,
    /// the addresses that have a large taker, or one let in to be one
    large: HashSet<IpAddr>,
    /// the takers that wait to be large, by address, the oldest first
    waiting: HashMap<IpAddr, VecDeque<u64>>,
    /// the addresses that have takers waiting, in the order their turns come
    turns: VecDeque<IpAddr>,
    /// takers let in to be large that have not yet seen it
    let_in: HashSet<u64>,
    /// the number the next taker that waits is known by
    next: u64,
    /// what a large taker another waits for sends at the least, and over what time
    pace: (u64, Duration),
}

impl Held {
    /// Lets in as many of the takers that wait as there is room for large
    /// ones, the oldest of an address at a time, the addresses in turn.
    fn let_in(&mut self, most: usize) {
        let mut passed = 0;
        while self.large.len() < most && passed < self.turns.len() {
            let from = self.turns.pop_front().expect("a turn");
            if self.large.contains(&from) {
                self.turns.push_back(from);
                passed += 1;
                continue;
            }
            let queue = self.waiting.get_mut(&from).expect("takers that wait");
            let taker = queue.pop_front().expect("a taker");
            if queue.is_empty() {
                self.waiting.remove(&from);
            } else {
                self.turns.push_back(from);
            }
            self.large.insert(from);
            self.let_in.insert(taker);
            passed = 0;
        }
    }

    /// the taker `taker` of `from` waits no more, and was not let in
    fn waits_no_more(&mut self, from: IpAddr, taker: u64) {
        let queue = self.waiting.get_mut(&from).expect("takers that wait");
        queue.retain(|waiting| *waiting != taker);
        if queue.is_empty() {
            self.waiting.remove(&from);
            self.turns.retain(|turn| *turn != from);
        }
    }
}

pub struct Shares {
    most: u64,
    /// a taker that is alone has no limit
    alone: bool,
    /// the large takers there may be at once
    large: usize,
    held: Mutex<Held>,
    /// told whenever a large taker is gone or one that waits is let in
    changed: Condvar,
}

/// What one taker has, given back when it is gone.
pub struct Share {
    of: Arc<Shares>,
    /// the address it counts for; none for this host
    from: Option<IpAddr>,
    bytes: u64,
    /// the one of its address that has more than a part
    large: bool,
    /// how long it waits for its turn to be large; it does not where none
    waits: Option<Wait>,
    /// since when it is sent in the window that is running, and what it sent in it
    sending: Option<(Instant, u64)>,
}

struct Wait {
    by: Option<Instant>,
    gone: Option<Arc<AtomicBool>>,
}

impl Wait {
    fn goes_on(&self) -> bool {
        let gone = self
            .gone
            .as_ref()
            .is_some_and(|gone| gone.load(Ordering::Relaxed));
        !gone && self.by.is_none_or(|by| Instant::now() < by)
    }
}

/// there is no room for what a taker asked for
#[derive(Debug)]
pub struct Full;

/// a large taker that another waits for is sent too slowly
#[derive(Debug, PartialEq)]
pub struct Slow;

impl std::fmt::Display for Slow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a large response that another waits for is taken too slowly")
    }
}

impl std::error::Error for Slow {}

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
        Arc::new(Shares {
            most,
            alone,
            large,
            held,
            changed,
        })
    }

    /// The large takers another waits for send `rate` bytes a second in each
    /// `window` at the least, and not [`LARGE_RATE`] in [`LARGE_WINDOW`].
    pub fn paced(self: Arc<Self>, rate: u64, window: Duration) -> Arc<Self> {
        self.lock().pace = (rate, window);
        self
    }

    /// A taker for the address `from`, which has nothing yet.
    pub fn share(self: &Arc<Self>, from: Option<IpAddr>) -> Share {
        let (of, bytes, large, waits, sending) = (Arc::clone(self), 0, false, None, None);
        Share {
            of,
            from,
            bytes,
            large,
            waits,
            sending,
        }
    }

    /// the amount there is for all the takers
    pub fn most(&self) -> u64 {
        self.most
    }

    /// what all the takers there are have between them
    pub fn held(&self) -> u64 {
        self.lock().all
    }

    /// the takers that wait for their turn to be large
    pub fn waiting(&self) -> usize {
        self.lock().waiting.values().map(VecDeque::len).sum()
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
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

    /// The taker waits for its turn to be large until `by`, and while `gone`
    /// is not set.
    pub fn waiting(mut self, by: Option<Instant>, gone: Option<Arc<AtomicBool>>) -> Self {
        self.waits = Some(Wait { by, gone });
        self
    }

    /// Whether the taker is a large one that another waits for: one that would
    /// be let in once it is gone, being of its address or of one that has no
    /// large taker.
    pub fn waited_for(&self) -> bool {
        let (Some(from), true) = (self.from, self.large) else {
            return false;
        };
        let held = self.of.lock();
        (held.waiting.keys()).any(|waiting| *waiting == from || !held.large.contains(waiting))
    }

    /// The peer took `bytes` more of what the taker holds. Where a window is
    /// over in which the taker was large, another waited for it, and fewer
    /// bytes than its pace asks were taken, it is [`Slow`].
    pub fn sent(&mut self, bytes: u64) -> Result<(), Slow> {
        self.sent_at(bytes, Instant::now())
    }

    fn sent_at(&mut self, bytes: u64, now: Instant) -> Result<(), Slow> {
        let (since, moved) = self.sending.get_or_insert((now, 0));
        *moved += bytes;
        let (rate, window) = self.of.lock().pace;
        let over = now.duration_since(*since);
        if over < window {
            return Ok(());
        }
        let slow = (*moved as u128) * 1000 < rate as u128 * over.as_millis();
        self.sending = Some((now, 0));
        match slow && self.waited_for() {
            true => Err(Slow),
            false => Ok(()),
        }
    }

    /// Takes `bytes` more, or nothing where they are not there to be taken.
    /// A taker that would be large waits for its turn as long as it is told.
    pub fn take(&mut self, bytes: u64) -> Result<(), Full> {
        let of = Arc::clone(&self.of);
        let mut held = of.lock();
        let (most, part) = (of.most, of.most / PARTS);
        let mine = self.bytes + bytes;
        let alone = |held: &Held| of.alone && held.all + bytes == mine;
        let over = |held: &Held| held.all + bytes > most && !alone(held);
        if over(&held) {
            return Err(Full);
        }
        let Some(from) = self.from else {
            held.all += bytes;
            self.bytes = mine;
            return Ok(());
        };
        if self.large || mine <= part {
            if self.large && mine > most / 2 && !alone(&held) {
                return Err(Full);
            }
            if !self.large {
                let theirs = held.by.get(&from).copied().unwrap_or(0) + bytes;
                if theirs > part {
                    return Err(Full);
                }
                held.by.insert(from, theirs);
            }
            held.all += bytes;
            self.bytes = mine;
            return Ok(());
        }
        // only a taker alone may be larger than half: one that is not now waits in vain
        if mine > most / 2 && !of.alone {
            return Err(Full);
        }
        held = self.turn(held, from)?;
        if over(&held) || (mine > most / 2 && !alone(&held)) {
            held.large.remove(&from);
            held.let_in(of.large);
            of.changed.notify_all();
            return Err(Full);
        }
        let theirs = held.by.get(&from).copied().unwrap_or(0) - self.bytes;
        match theirs {
            0 => held.by.remove(&from),
            theirs => held.by.insert(from, theirs),
        };
        held.all += bytes;
        (self.bytes, self.large) = (mine, true);
        Ok(())
    }

    /// Waits for the turn of the taker to be large, as long as it is told to.
    fn turn<'a>(
        &self,
        mut held: MutexGuard<'a, Held>,
        from: IpAddr,
    ) -> Result<MutexGuard<'a, Held>, Full> {
        let taker = held.next;
        held.next += 1;
        held.waiting.entry(from).or_default().push_back(taker);
        if !held.turns.contains(&from) {
            held.turns.push_back(from);
        }
        held.let_in(self.of.large);
        loop {
            if held.let_in.remove(&taker) {
                return Ok(held);
            }
            if !self.waits.as_ref().is_some_and(Wait::goes_on) {
                held.waits_no_more(from, taker);
                return Err(Full);
            }
            held = self.of.changed.wait_timeout(held, LOOK).expect("shares").0;
        }
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        let mut held = self.of.lock();
        held.all -= self.bytes;
        let Some(from) = self.from else {
            return;
        };
        if self.large {
            held.large.remove(&from);
            held.let_in(self.of.large);
            self.of.changed.notify_all();
        } else if let Some(theirs) = held.by.get_mut(&from) {
            *theirs -= self.bytes;
            if *theirs == 0 {
                held.by.remove(&from);
            }
        }
    }
}

#[cfg(test)]
mod tests;
