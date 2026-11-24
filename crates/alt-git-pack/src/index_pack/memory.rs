//! The memory that packs being indexed at once may take between them.
//!
//! A pack has two kinds of memory. One it has for as long as it is worked
//! on: what is known of each entry, the bases that wait. The other it has
//! for a moment: a large object while it is inflated or made of a delta,
//! as its base, as the instructions and as the result. How large the
//! largest of those is, only the sender says before it is made.
//!
//! So a pack is let in with what it has throughout, and says the most one
//! of its objects takes. Nothing is taken for that: room for the largest
//! object any pack let in may make is kept free, once for all of them. A
//! walk that comes to a large object takes as much as the largest of its
//! pack takes, for as long as it has a large object, and so never needs
//! more than it has. Two packs that say they make an object as large as
//! one may be keep free what one of them does; what the packs leave free
//! beyond that, the objects being made have as well.
//!
//! Whoever finds no room waits in line, in the order they came: the packs
//! to be let in in one line, the walks that come to a large object in
//! another. One that comes later and has room is let in before one that
//! waits for more, a few times and no more. A pack waits to be let in while it has nothing, and a walk
//! while it has no large object, so none waits for what another that
//! waits has. While a pack waits to be let in, the objects are held to
//! what is kept free for them, so that it is let in when its turn comes.
//! Those that wait go on waiting no longer than until a moment they name,
//! nor than whoever sent their pack waits for it.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::PackError;

/// How long is waited for what others have.
#[derive(Debug, Clone, Copy, Default)]
pub struct Wait<'a> {
    /// no longer than until then
    pub by: Option<Instant>,
    /// set once whoever sent the pack waits for it no longer
    pub gone: Option<&'a AtomicBool>,
}

impl<'a> Wait<'a> {
    pub fn new(by: Option<Instant>, gone: Option<&'a AtomicBool>) -> Self {
        Wait { by, gone }
    }
}

/// how often one that waits looks whether whoever sent its pack is gone
const LOOK: Duration = Duration::from_millis(100);

/// How many that came later and have room are let in before one that waits for
/// more than there is: a small pack does not wait out a large one, and a large
/// one is not kept waiting by small ones without end.
const PASSED: u32 = 8;
const PACKS: usize = 0;
const OBJECTS: usize = 1;

#[derive(Debug)]
pub struct Memory {
    all: u64,
    held: Mutex<Held>,
    given_back: Condvar,
}

#[derive(Debug)]
struct Held {
    /// what the packs let in have for as long as they are worked on
    packs: u64,
    /// what the objects being made have
    objects: u64,
    /// the most one object of a pack let in takes, and how many packs say so
    most: BTreeMap<u64, usize>,
    /// those that wait, the packs and the objects, in the order they came,
    /// each with how many that came later were let in before it
    lines: [VecDeque<(u64, u32)>; 2],
    came: u64,
}

impl Held {
    /// Whether the one at `place` in `line`, or one that has just come, may be
    /// let in now: none before it has been passed as often as one may be. Those
    /// before it are passed once more where it may.
    fn turn(&mut self, line: usize, place: Option<u64>) -> bool {
        let line = &mut self.lines[line];
        let before = match place {
            Some(place) => line.iter().take_while(|(other, _)| *other != place).count(),
            None => line.len(),
        };
        let may = line.iter().take(before).all(|(_, passed)| *passed < PASSED);
        if may {
            (line.iter_mut().take(before)).for_each(|(_, passed)| *passed += 1);
        }
        may
    }

    /// what is kept free of the packs for the objects being made
    fn kept_free(&self) -> u64 {
        let said = self.most.last_key_value().map_or(0, |(most, _)| *most);
        said.max(self.objects)
    }
}

/// A part of a [`Memory`], given back when it is dropped: what a pack has
/// throughout, or what one object has.
#[derive(Debug)]
pub struct Taken<'a> {
    of: &'a Memory,
    bytes: u64,
    /// of a pack: the most one of its objects takes
    most: u64,
    object: bool,
}

impl Memory {
    pub const fn new(bytes: u64) -> Self {
        Memory {
            all: bytes,
            held: Mutex::new(Held {
                packs: 0,
                objects: 0,
                most: BTreeMap::new(),
                lines: [VecDeque::new(), VecDeque::new()],
                came: 0,
            }),
            given_back: Condvar::new(),
        }
    }

    /// Takes `bytes`, once the packs that have them are done. A pack that
    /// needs more than there is at all is refused.
    pub fn take(&self, bytes: u64) -> Result<Taken<'_>, PackError> {
        self.let_in(bytes, 0, Wait::default())
    }

    /// [`Self::take`], waiting no longer than until `by`: a pack the
    /// others leave no room for by then is told that they are busy.
    pub fn take_by(&self, bytes: u64, by: Option<Instant>) -> Result<Taken<'_>, PackError> {
        self.let_in(bytes, 0, Wait { by, gone: None })
    }

    /// Lets a pack in that has `bytes` throughout and whose largest object
    /// takes `object` while it is made, when its turn has come and there is
    /// room for both. One there is no room for at all is refused.
    pub fn let_in(&self, bytes: u64, object: u64, wait: Wait<'_>) -> Result<Taken<'_>, PackError> {
        if bytes.saturating_add(object) > self.all {
            return Err(PackError::Limit("index memory"));
        }
        let room = |held: &Held| held.packs + bytes + held.kept_free().max(object) <= self.all;
        let mut held = self.in_turn(PACKS, wait, room)?;
        held.packs += bytes;
        *held.most.entry(object).or_default() += 1;
        Ok(Taken {
            of: self,
            bytes,
            most: object,
            object: false,
        })
    }

    /// All there is, free or taken.
    pub fn all(&self) -> u64 {
        self.all
    }

    /// What the packs let in have throughout, and what the objects being
    /// made have.
    pub fn taken(&self) -> (u64, u64) {
        let held = self.held.lock().expect("index memory");
        (held.packs, held.objects)
    }

    /// How many wait to be let in, and how many for an object.
    pub fn waiting(&self) -> (usize, usize) {
        let held = self.held.lock().expect("index memory");
        (held.lines[PACKS].len(), held.lines[OBJECTS].len())
    }

    /// Waits in `line` until its turn has come and there is `room`.
    fn in_turn(
        &self,
        line: usize,
        wait: Wait<'_>,
        room: impl Fn(&Held) -> bool,
    ) -> Result<MutexGuard<'_, Held>, PackError> {
        let mut held = self.held.lock().expect("index memory");
        if room(&held) && held.turn(line, None) {
            return Ok(held);
        }
        let place = held.came;
        held.came += 1;
        held.lines[line].push_back((place, 0));
        loop {
            if room(&held) && held.turn(line, Some(place)) {
                held.lines[line].retain(|(other, _)| *other != place);
                // the next in line may have room as well
                self.given_back.notify_all();
                return Ok(held);
            }
            let gone = wait.gone.is_some_and(|gone| gone.load(Relaxed));
            let left = wait
                .by
                .map(|by| by.saturating_duration_since(Instant::now()));
            if gone || left.is_some_and(|left| left.is_zero()) {
                held.lines[line].retain(|(other, _)| *other != place);
                self.given_back.notify_all();
                return Err(match gone {
                    true => PackError::GivenUp,
                    false => PackError::Busy("index memory"),
                });
            }
            let look = wait.gone.map(|_| LOOK);
            held = match look.into_iter().chain(left).min() {
                None => self.given_back.wait(held).expect("index memory"),
                Some(pause) => {
                    let waited = self.given_back.wait_timeout(held, pause);
                    waited.expect("index memory").0
                }
            };
        }
    }
}

impl<'a> Taken<'a> {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// of a pack: the most one of its objects takes
    pub fn most(&self) -> u64 {
        self.most
    }

    /// Gives back all but `bytes` of what was taken.
    pub fn keep(&mut self, bytes: u64) {
        let back = self.bytes.saturating_sub(bytes);
        self.bytes -= back;
        let mut held = self.of.held.lock().expect("index memory");
        match self.object {
            true => held.objects -= back,
            false => held.packs -= back,
        }
        self.of.given_back.notify_all();
    }

    /// Makes what a pack has throughout `bytes`, and the most one of its
    /// objects takes `object`, where there is room for that now. A pack that
    /// is let in is not sent back into the line for what it finds it needs.
    pub fn have_now(&mut self, bytes: u64, object: u64) -> bool {
        let mut held = self.of.held.lock().expect("index memory");
        let others = held.packs - self.bytes;
        let said = held.most.iter().rev().find_map(|(most, packs)| {
            let beside = *most != self.most || *packs > 1;
            beside.then_some(*most)
        });
        let free = said.unwrap_or(0).max(held.objects).max(object);
        if others + bytes + free > self.of.all {
            return false;
        }
        held.packs = others + bytes;
        leave(&mut held.most, self.most);
        *held.most.entry(object).or_default() += 1;
        (self.bytes, self.most) = (bytes, object);
        self.of.given_back.notify_all();
        true
    }

    /// Takes what the largest object of this pack takes, for a walk that
    /// has come to a large one, when its turn has come: out of what the
    /// packs leave free, and out of what is kept free for the objects alone
    /// while a pack waits to be let in.
    pub fn object(&self, wait: Wait<'_>) -> Result<Taken<'a>, PackError> {
        let (of, bytes) = (self.of, self.most);
        let room = |held: &Held| {
            let said = held.most.last_key_value().map_or(0, |(most, _)| *most);
            let free = match held.lines[PACKS].is_empty() {
                true => of.all - held.packs,
                false => said,
            };
            held.objects + bytes <= free
        };
        let mut held = of.in_turn(OBJECTS, wait, room)?;
        held.objects += bytes;
        Ok(Taken {
            of,
            bytes,
            most: 0,
            object: true,
        })
    }
}

/// one pack fewer says that its largest object takes `most`
fn leave(said: &mut BTreeMap<u64, usize>, most: u64) {
    let packs = said.get_mut(&most).expect("said when let in");
    *packs -= 1;
    if *packs == 0 {
        said.remove(&most);
    }
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        let mut held = self.of.held.lock().expect("index memory");
        match self.object {
            true => held.objects -= self.bytes,
            false => {
                held.packs -= self.bytes;
                leave(&mut held.most, self.most);
            }
        }
        self.of.given_back.notify_all();
    }
}
