use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, MutexGuard};
use std::time::Instant;

use crate::held::Held;
use crate::{LOOK, PARTS, Shares};

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
    /// what it gives back while it waits for its turn
    aside: Option<Box<dyn Aside>>,
    /// it does not wait for its turn while it is taken of: it is put off, to
    /// wait with nothing of what it was being built from held
    first: bool,
    /// it was put off where it would have waited
    put_off: bool,
}

/// What a taker gives back while it waits for its turn to be large, and
/// takes back once it has it: the worker of a server, which serves others
/// meanwhile.
pub trait Aside: Send {
    fn step_aside(&mut self);
    /// Takes back what was given back, for as long as the taker is waited
    /// for: false where it was not had again.
    fn step_back(&mut self) -> bool;
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

/// A taker counted as being built while this is held: from when what it is
/// built of is first taken, such as the objects a response is chosen from,
/// to when it is all written or dropped.
pub struct Building(Arc<Shares>);

impl Drop for Building {
    fn drop(&mut self) {
        self.0.building.fetch_sub(1, Ordering::Relaxed);
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

impl Share {
    pub(crate) fn new(of: Arc<Shares>, from: Option<IpAddr>) -> Self {
        let (bytes, large, waits, sending) = (0, false, None, None);
        Share {
            of,
            from,
            bytes,
            large,
            waits,
            sending,
            aside: None,
            first: false,
            put_off: false,
        }
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The taker waits for its turn to be large until `by`, and while `gone`
    /// is not set.
    pub fn waiting(mut self, by: Option<Instant>, gone: Option<Arc<AtomicBool>>) -> Self {
        self.waits = Some(Wait { by, gone });
        self
    }

    /// While the taker waits for its turn, `aside` is given back.
    pub fn aside(mut self, aside: Box<dyn Aside>) -> Self {
        self.aside = Some(aside);
        self
    }

    /// The taker has its turn to be large before it is built: where it would
    /// wait for it, [`Share::take`] and [`Share::expect`] put it off instead,
    /// and what is built is dropped and built again once [`Share::take_turn`]
    /// is through. What waits for a turn then holds no more than the request
    /// it answers, and those that are built at once are no more than the
    /// workers that build them.
    pub fn turn_first(mut self) -> Self {
        self.first = true;
        self
    }

    /// The taker counted under [`Shares::building`] while what is given is held.
    pub fn building(&self) -> Building {
        self.of.building.fetch_add(1, Ordering::Relaxed);
        Building(Arc::clone(&self.of))
    }

    /// whether the taker was put off where it would have waited for its turn
    pub fn put_off(&self) -> bool {
        self.put_off
    }

    /// The taker is to take about `bytes` more: where that makes it large, it
    /// has its turn now, as [`Share::take`] would. One whose address has all
    /// its part has none, and is refused, not put off: [`Share::take`] would
    /// refuse what it took first.
    pub fn expect(&mut self, bytes: u64) -> Result<(), Full> {
        let of = Arc::clone(&self.of);
        let (Some(from), false) = (self.from, self.large) else {
            return Ok(());
        };
        let part = of.most / PARTS;
        if self.bytes + bytes <= part {
            return Ok(());
        }
        let held = of.lock();
        if held.by.get(&from).copied().unwrap_or(0) - self.bytes >= part {
            return Err(Full);
        }
        let mut held = self.turn(&of, held, from)?;
        self.large_now(&mut held, from);
        Ok(())
    }

    /// The taker gives back what it took and waits for its turn to be large,
    /// as long as it is told and with what it gives back meanwhile given back:
    /// what it is built from is built again once it has it.
    pub fn take_turn(&mut self) -> Result<(), Full> {
        self.put_off = false;
        let of = Arc::clone(&self.of);
        let Some(from) = self.from else {
            return Ok(());
        };
        let mut held = of.lock();
        held.all -= self.bytes;
        if !self.large {
            let theirs = held.by.get(&from).copied().unwrap_or(0) - self.bytes;
            match theirs {
                0 => held.by.remove(&from),
                theirs => held.by.insert(from, theirs),
            };
        }
        self.bytes = 0;
        if self.large {
            return Ok(());
        }
        self.first = false;
        let turned = self.turn(&of, held, from);
        self.first = true;
        let mut held = turned?;
        self.large_now(&mut held, from);
        Ok(())
    }

    /// Whether the taker is a large one that a taker of another address waits
    /// for: one that would be let in once it is gone, its address having no
    /// large taker. One of its own address that waits does not hurry it: the
    /// two would only take turns at being let go of.
    pub fn waited_for(&self) -> bool {
        let (Some(from), true) = (self.from, self.large) else {
            return false;
        };
        let held = self.of.lock();
        (held.waiting.keys()).any(|waiting| *waiting != from && !held.large.contains(waiting))
    }

    /// The peer took `bytes` more of what the taker holds. Where a window is
    /// over in which the taker was large, another waited for it, and fewer
    /// bytes than its pace asks were taken, it is [`Slow`].
    pub fn sent(&mut self, bytes: u64) -> Result<(), Slow> {
        self.sent_at(bytes, Instant::now())
    }

    pub(crate) fn sent_at(&mut self, bytes: u64, now: Instant) -> Result<(), Slow> {
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
        held = self.turn(&of, held, from)?;
        if over(&held) || (mine > most / 2 && !alone(&held)) {
            held.large.remove(&from);
            held.let_in(of.large);
            of.changed.notify_all();
            return Err(Full);
        }
        self.large_now(&mut held, from);
        held.all += bytes;
        self.bytes = mine;
        Ok(())
    }

    /// the taker is large, let in to be: what it holds is no more its address's part
    fn large_now(&mut self, held: &mut Held, from: IpAddr) {
        let theirs = held.by.get(&from).copied().unwrap_or(0) - self.bytes;
        match theirs {
            0 => held.by.remove(&from),
            theirs => held.by.insert(from, theirs),
        };
        self.large = true;
    }

    /// Waits for the turn of the taker to be large, as long as it is told to,
    /// with what it gives back while it waits given back.
    fn turn<'a>(
        &mut self,
        of: &'a Shares,
        mut held: MutexGuard<'a, Held>,
        from: IpAddr,
    ) -> Result<MutexGuard<'a, Held>, Full> {
        let taker = held.next;
        held.next += 1;
        held.waiting.entry(from).or_default().push_back(taker);
        if !held.turns.contains(&from) {
            held.turns.push_back(from);
        }
        held.let_in(of.large);
        let mut aside = false;
        loop {
            if held.let_in.remove(&taker) {
                if !aside {
                    return Ok(held);
                }
                drop(held);
                let back = self.aside.as_mut().is_some_and(|aside| aside.step_back());
                held = of.lock();
                if back {
                    return Ok(held);
                }
                held.large.remove(&from);
                held.let_in(of.large);
                of.changed.notify_all();
                return Err(Full);
            }
            if self.first {
                held.waits_no_more(from, taker);
                self.put_off = true;
                return Err(Full);
            }
            if !self.waits.as_ref().is_some_and(Wait::goes_on) {
                held.waits_no_more(from, taker);
                return Err(Full);
            }
            if !aside && let Some(given) = self.aside.as_mut() {
                given.step_aside();
                aside = true;
            }
            held = of.changed.wait_timeout(held, LOOK).expect("shares").0;
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
