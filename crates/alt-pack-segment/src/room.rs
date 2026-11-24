//! The memory a pack takes out of what the packs verified at once have between them.
//!
//! A pack has some of it for as long as it is verified: what is known of each
//! entry it is said to have, no more of them than its bytes can hold; the payloads
//! the scan keeps, taken as they are kept and not kept where there is no room; the
//! bases that wait, and what is told of each object a delta made; and for each walk
//! what a small object takes while it is made. For a large object, inflated by the
//! scan or made of a delta, memory is taken while a large one is there: as the base
//! it is made of, as the instructions of the delta and as the result, whose buffer
//! grows to twice its size at the most. The pack says how much the largest takes
//! when it is let in, and as much is kept free once for all the packs.

use crate::{
    Error, Limits,
    scan::{Form, Scanned},
};
use alt_git_pack::{
    Taken, Wait,
    walk::{Budget, ObjectMemory},
};

/// What verifying a pack holds for each of its deltas, beside the objects: what
/// the walk that resolved it tells of the object it made.
pub const DELTA_BYTES: u64 = std::mem::size_of::<(usize, crate::ingest::Meta)>() as u64;

/// What verifying a pack holds for each of its entries from when it is scanned to
/// when its index is written: the entry as it was scanned, what it was found to
/// be, its record, and its places in the orders the records are put in.
pub const ENTRY_BYTES: u64 = (std::mem::size_of::<crate::scan::Raw>()
    + std::mem::size_of::<Option<crate::ingest::Meta>>()
    + std::mem::size_of::<crate::Record>()
    + 2 * std::mem::size_of::<usize>()
    + std::mem::size_of::<u32>()) as u64;

/// what one byte of a pack inflates to at the most
const DEFLATE_MOST: u64 = 1032;
/// the fewest bytes an entry has: a byte of header and the stream of nothing
pub(crate) const ENTRY_LEAST: u64 = 9;
/// the payloads the scan keeps are taken for so many bytes at a time
const KEPT_STEP: u64 = 4 << 20;

/// The least memory a pack can be verified in that holds an object as large as one
/// may be: scanned, and then resolved one delta at a time.
pub fn least_memory(limits: &Limits) -> u64 {
    let scanned = crate::ingest::HELD.saturating_add(limits.object_bytes);
    let resolved = (limits.base_bytes as u64).saturating_add(limits.object_bytes.saturating_mul(4));
    scanned.max(resolved.saturating_add(crate::ingest::HELD))
}

/// what an object takes of the memory kept free for objects: nothing where a walk
/// has as much throughout
fn large(bytes: u64, limits: &Limits) -> u64 {
    if bytes > limits.each_bytes { bytes } else { 0 }
}

/// The memory of a pack while it is verified.
pub(crate) struct Room<'a> {
    taken: Option<Taken<'static>>,
    wait: Wait<'a>,
    each: u64,
    /// what the pack has throughout beside the payloads kept
    fixed: u64,
    /// the most bytes of payloads that are kept
    budget: u64,
    /// the bytes of payloads there is memory for, and those kept; whether none
    /// was there for one more
    room: u64,
    kept: u64,
    full: bool,
    /// the memory of the entry being inflated, where it is a large one
    entry: Option<Taken<'static>>,
}

/// Lets the pack in to be scanned. `entries` is how many it says it has, and
/// `budget` the most bytes of payloads that are kept.
pub(crate) fn scanning<'a>(
    pack_bytes: u64,
    entries: u64,
    limits: &Limits,
    (budget, wait): (u64, Wait<'a>),
) -> Result<Room<'a>, Error> {
    if entries > limits.objects {
        return Err(Error::Limit("objects"));
    }
    let table = entries.min(pack_bytes / ENTRY_LEAST) * ENTRY_BYTES;
    let fixed = table.saturating_add(limits.each_bytes);
    let entry = pack_bytes
        .saturating_mul(DEFLATE_MOST)
        .min(limits.object_bytes);
    let taken = match limits.memory {
        Some(memory) => Some(memory.let_in(fixed, large(entry, limits), wait)?),
        None => None,
    };
    Ok(Room {
        taken,
        wait,
        each: limits.each_bytes,
        fixed,
        budget,
        room: 0,
        kept: 0,
        full: false,
        entry: None,
    })
}

impl<'a> Room<'a> {
    /// Memory for an entry that inflates to `bytes`, in place of what the one
    /// before it had.
    pub(crate) fn inflating(&mut self, bytes: u64) -> Result<(), Error> {
        if let Some(objects) = ObjectMemory::of(&self.taken, self.each) {
            objects.room(&mut self.entry, bytes, self.wait)?;
        }
        Ok(())
    }

    /// Whether a payload of `bytes` may be kept: no more are kept with it than may
    /// be, and there is memory for it, or more is there to be taken now. Nothing is
    /// waited for to keep a payload.
    pub(crate) fn keeping(&mut self, bytes: u64) -> bool {
        if self.kept + bytes > self.budget {
            return false;
        }
        if let Some(taken) = &mut self.taken
            && self.kept + bytes > self.room
        {
            let needed = self.kept + bytes - self.room;
            let has = self.fixed + self.room;
            let most = taken.most();
            let more = [needed.max(KEPT_STEP), needed]
                .into_iter()
                .find(|more| !self.full && taken.have_now(has + more, most));
            let Some(more) = more else {
                self.full = true;
                return false;
            };
            self.room += more;
        }
        self.kept += bytes;
        true
    }

    /// The memory the pack is resolved in, how many walks may resolve at once, and
    /// what each has for the objects it makes. Where others leave too little for
    /// what the scan kept and the walks beside it, what the scan kept is let go
    /// of, to be inflated again where it is needed, and the pack goes back into
    /// the line with what it needs at the least.
    pub(crate) fn resolving(
        mut self,
        limits: &Limits,
        scanned: &mut Scanned,
    ) -> Result<Resolving<'a>, Error> {
        self.entry = None;
        let wait = self.wait;
        let Some(mut taken) = self.taken.take() else {
            let (walks, taken, each) = (None, None, 0);
            return Ok(Resolving {
                walks,
                taken,
                each,
                wait,
            });
        };
        let table = (scanned.entries.len() as u64).saturating_mul(ENTRY_BYTES);
        // each delta is resolved once, and told of once
        let deltas = (scanned.entries.iter())
            .filter(|raw| !matches!(raw.form, Form::Plain(_)))
            .count() as u64;
        let bases = (limits.base_bytes as u64).saturating_add(deltas.saturating_mul(DELTA_BYTES));
        let object = large(scanned.largest.saturating_mul(4), limits);
        let each = limits.each_bytes.min(scanned.largest.saturating_mul(4));
        let threads = std::thread::available_parallelism().map_or(1, |count| count.get() as u64);
        let with = |kept: u64, walks: u64| {
            (table.saturating_add(bases))
                .saturating_add(kept)
                .saturating_add(walks.saturating_mul(each))
        };
        let mut walks = threads;
        if !taken.have_now(with(scanned.held, walks), object) {
            for raw in &mut scanned.entries {
                raw.data = None;
            }
            scanned.held = 0;
            walks = 1;
            if !taken.have_now(with(0, 1), object) {
                drop(taken);
                let memory = limits.memory.expect("taken of it");
                taken = memory.let_in(with(0, 1), object, self.wait)?;
            }
        }
        Ok(Resolving {
            walks: Some(walks as usize),
            taken: Some(taken),
            each,
            wait: self.wait,
        })
    }
}

/// The memory a pack is resolved in.
pub(crate) struct Resolving<'a> {
    /// how many walks resolve at once, where the memory bounds them
    pub walks: Option<usize>,
    taken: Option<Taken<'static>>,
    each: u64,
    wait: Wait<'a>,
}

impl<'a> Resolving<'a> {
    /// What is kept of a pack once its deltas are resolved: of its memory, as
    /// much as `entries` and what is known of each take.
    pub(crate) fn keeping(mut self, entries: &[crate::scan::Raw]) -> Option<Taken<'static>> {
        let mut taken = self.taken.take()?;
        taken.keep((entries.len() as u64).saturating_mul(ENTRY_BYTES + DELTA_BYTES));
        Some(taken)
    }

    /// Tells the walks where their memory is, how long they wait, and where the
    /// bases in files go.
    pub(crate) fn told_to(
        &'a self,
        budget: &mut Budget<'a>,
        files: Option<&'static alt_git_pack::BaseFiles>,
    ) {
        budget.objects = ObjectMemory::of(&self.taken, self.each);
        (budget.files, budget.wait) = (files, self.wait);
    }
}
