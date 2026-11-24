//! alt operation log: the transactional spine of the `.alt` state layer.
//!
//! Every state change is one op appended to `oplog/log`. An op's id is the
//! BLAKE3 of its body, and each body names its parent's id, so the log is
//! a hash chain: replaying from the root reproduces (and verifies) the
//! exact sequence of operations. Undo is a later op that moves state
//! back, never a rewrite — the log itself only ever grows.
//!
//! The file comes in two formats, told apart by the version in its header:
//! [`v1`], records one after another with a marker file beside the log
//! that says how far it is synced, and [`v2`], frames in blocks that carry
//! that boundary themselves. A handle opens whichever it finds.
//!
//! Pure-logic crate, business-agnostic: payloads are opaque bytes; what an
//! op *means* (ref updates, imports, …) is the caller's encoding.

mod bound;
mod handle;
mod left;
mod lock;
mod note;
mod op;
mod read;
mod scan;
mod settle;
mod since;
mod sink;
mod sync;
mod synced;
mod tail;
mod v1;
pub mod v2;

pub use handle::{OpLog, OpLogSink, Reached};
pub use note::{NOTE_LEN, NotePlace, encode_note, parse_note};
pub use op::{Framed, Op, OpId, OpLogError, ROOT};
pub use settle::{Unconfirmed, end_reads_at, passed_damage, reading_past_damage};
pub use sync::relaxed_durability;
pub use synced::SyncNamed;
pub use tail::Discarded;
pub use v2::{Answer, Meta, Ticket, Writing};

/// A place in the log to start reading from instead of its first record:
/// the op whose record occupies bytes `start..end`, being op number `count`
/// of the chain (counting from one). Whoever derives state from the log up
/// to an op (the ref snapshot) keeps one, so a later open replays only what
/// came after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub op: OpId,
    pub start: u64,
    pub end: u64,
    pub count: u64,
}

/// The boundary the log in `dir` is known synced up to, for whoever has
/// not opened it yet: `None` where nothing records one.
pub fn bounded_in(dir: &std::path::Path) -> Option<u64> {
    match handle::format_of(dir) {
        Ok(Some(v2::block::VERSION)) => v2::bounded_in(dir),
        _ => sync::bounded_in(dir),
    }
}
