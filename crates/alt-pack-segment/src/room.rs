//! The memory a pack takes out of what the packs verified at once have between them.
//!
//! Scanned, a pack has in memory what the scan keeps and the one entry being
//! inflated. Resolved, it has what the scan kept, the bases that wait, and for each
//! walk below a base the object being made: as the base it is made of, as the
//! instructions of the delta and as the result, whose buffer grows to twice its
//! size at the most, and for each delta what is told of the object it made. A pack
//! takes each amount before it allocates, and waits for none while it holds
//! another.

use crate::{
    Error, Limits,
    scan::{Form, Scanned},
};
use alt_git_pack::Taken;

type Room = Option<Taken<'static>>;

/// What verifying a pack holds for each of its deltas, beside the objects: what
/// the walk that resolved it tells of the object it made.
pub const DELTA_BYTES: u64 = std::mem::size_of::<(usize, crate::ingest::Meta)>() as u64;

/// what one byte of a pack inflates to at the most
const DEFLATE_MOST: u64 = 1032;

/// The least memory a pack can be verified in that holds an object as large as one
/// may be: scanned, and then resolved one delta at a time.
pub fn least_memory(limits: &Limits) -> u64 {
    let scanned = crate::ingest::HELD.saturating_add(limits.object_bytes);
    let resolved = (limits.base_bytes as u64).saturating_add(limits.object_bytes.saturating_mul(4));
    scanned.max(resolved.saturating_add(crate::ingest::HELD))
}

pub(crate) fn scanning(pack_bytes: u64, limits: &Limits, held: u64) -> Result<Room, Error> {
    let Some(memory) = limits.memory else {
        return Ok(None);
    };
    let body = pack_bytes.saturating_mul(DEFLATE_MOST);
    let needed = body.min(held).saturating_add(body.min(limits.object_bytes));
    Ok(Some(memory.take_by(needed, limits.memory_by)?))
}

/// How many walks may resolve at once, where the memory there is bounds them, and
/// the memory they take. Where others leave too little for what the scan kept and
/// the walks beside it, what the scan kept is let go of, to be inflated again where
/// it is needed, and one walk waits for its room.
pub(crate) fn resolving(
    limits: &Limits,
    scanned: &mut Scanned,
    scanning: Room,
) -> Result<(Option<usize>, [Room; 2]), Error> {
    let (Some(memory), Some(mut kept)) = (limits.memory, scanning) else {
        return Ok((None, [None, None]));
    };
    kept.keep(scanned.held);
    // each delta is resolved once, and told of once
    let deltas = (scanned.entries.iter())
        .filter(|raw| !matches!(raw.form, Form::Plain(_)))
        .count() as u64;
    let bases = (limits.base_bytes as u64).saturating_add(deltas.saturating_mul(DELTA_BYTES));
    let walk = scanned.largest.saturating_mul(4);
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get() as u64);
    let room = (memory.all().saturating_sub(scanned.held)).saturating_sub(bases);
    let walks = room
        .checked_div(walk)
        .map_or(threads, |fit| fit.min(threads));
    if walks > 0
        && let Some(more) = memory.take_now(bases.saturating_add(walks.saturating_mul(walk)))
    {
        return Ok((Some(walks as usize), [Some(kept), Some(more)]));
    }
    for raw in &mut scanned.entries {
        raw.data = None;
    }
    scanned.held = 0;
    drop(kept);
    let alone = memory.take_by(bases.saturating_add(walk), limits.memory_by)?;
    Ok((Some(1), [Some(alone), None]))
}
