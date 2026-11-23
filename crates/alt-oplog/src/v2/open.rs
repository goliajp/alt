//! Reading a log for an open: where the read begins, whether what it
//! finds there is taken on trust, and what lies past the last frame.
//!
//! A checkpoint spares reading the frames before it. One that lies within
//! what the log says is synced is as it was written. One past that is in
//! the kernel's cache, whole for as long as the boot that wrote it runs:
//! it is begun at when it was written in the running boot, or when `tail`
//! says the frames up to it were read through in this boot. Otherwise the
//! machine went down since, frames before it may be gone, and the read
//! begins at a checkpoint within the synced part instead and goes through
//! everything after it.

use std::path::Path;

use crate::scan::parse_body;
use crate::v2::block::{FILE_HEADER_LEN, Layout};
use crate::v2::frame::{KIND_OP, View};
use crate::v2::locate::locate_past;
use crate::v2::salvage::salvage;
use crate::v2::scan::{Located, Stop, Walk};
pub(crate) use crate::v2::source::Source;
use crate::v2::state::State;
use crate::v2::stream::{ReadAt, walk_file};
use crate::v2::tail::Tail;
use crate::{Framed, Op, OpLogError};

/// The op an op frame at `at..end` holds.
pub(crate) fn op_of(at: u64, end: u64, view: &View) -> Result<Op, OpLogError> {
    let Some(id) = view.op else {
        return Err(OpLogError::Format("an op frame without an id"));
    };
    let mut op = parse_body(view.body, id)?;
    op.frame = Some(Framed {
        at,
        end,
        seq: view.head.seq,
        flags: view.head.flags,
        workspace: view.head.workspace,
        linked: view.ext.linked.is_some(),
        inverse_of: view.ext.inverse_of,
        request: view.ext.request,
    });
    Ok(op)
}

/// What an open read.
pub(crate) struct Loaded {
    pub state: State,
    pub ops: Vec<Op>,
    /// the four bytes of the file before where the last frame ends
    pub closes: [u8; 4],
    /// where the read began, and the last ref transaction before it
    pub began: u64,
    pub began_linked: u64,
    pub synced: u64,
    /// the ops whose frames start before this need no vouching for
    pub judged: u64,
    pub hidden: Option<u64>,
    /// how long the file was
    pub seen: u64,
    pub clamped: bool,
    pub passed: bool,
}

/// The `len` bytes of the file at `at`, out of `window` where it holds
/// them: bytes of the file from byte `window.0` on.
fn held(source: &Source, window: &(u64, Vec<u8>), at: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let skip = at.checked_sub(window.0).map(|skip| skip as usize);
    let within = skip.and_then(|skip| window.1.get(skip..skip.checked_add(len)?));
    match within {
        Some(bytes) => Ok(bytes.to_vec()),
        None => source.at(at, len),
    }
}

/// Reads on from `located` to byte `size` of the file: out of `window`
/// where that holds the bytes from there on, and a stretch of the file
/// at a time otherwise.
fn pass(
    source: &Source,
    layout: &Layout,
    located: &Located,
    size: u64,
    window: &(u64, Vec<u8>),
) -> Result<(State, Vec<Op>, Stop), OpLogError> {
    let mut state = located.state.clone();
    let mut ops = Vec::new();
    let mut each = |at: u64, end: u64, view: &View| {
        if view.head.kind == KIND_OP {
            ops.push(op_of(at, end, view)?);
        }
        Ok(())
    };
    let stop = match state.end >= window.0 {
        true => {
            let walk = Walk {
                layout: *layout,
                data: &window.1,
                base: window.0,
                strict: false,
            };
            walk.run(&mut state, &mut each)?
        }
        false => {
            let mut read = |at: u64, len: usize| source.at(at, len);
            walk_file(&mut read, layout, &mut state, size, false, &mut each)?
        }
    };
    Ok((state, ops, stop))
}

/// Whether what `tail` says is said of the log as this read found it.
fn told_of(tail: &Tail, state: &State, size: u64, window: &(u64, Vec<u8>)) -> bool {
    // the bytes before its boundary, where this read holds them
    let closes = tail
        .synced
        .checked_sub(4)
        .and_then(|at| at.checked_sub(window.0));
    let held = closes.and_then(|at| window.1.get(at as usize..)?.first_chunk::<4>());
    tail.tells(state.generation)
        && tail.judged <= size
        && held.is_none_or(|held| *held == tail.closes || tail.closes == [0; 4])
}

impl Loaded {
    /// Takes what follows the last frame, which is no frame, for what an
    /// append left unfinished or for damage: by whether the log is
    /// synced beyond where it starts. Whether damage was read past.
    fn stopped(
        &mut self,
        read: &mut ReadAt,
        layout: &Layout,
        (size, held): (u64, &[(u64, u64)]),
    ) -> Result<bool, OpLogError> {
        let (end, generation) = (self.state.end, self.state.generation);
        let found = salvage(read, layout, (end, size), generation, held)?;
        if end >= self.synced.max(found.claim) {
            self.passed = true;
            return Ok(false);
        }
        if !crate::settle::reading_past_damage() {
            return Err(OpLogError::Format("oplog record corrupt"));
        }
        self.clamped = true;
        Ok(true)
    }
}

/// A read of the file up to some byte: the checkpoint it began at, what
/// the frames from there on come to, and what `tail` says of that log.
struct Found {
    located: Located,
    state: State,
    ops: Vec<Op>,
    stop: Stop,
    tail: Option<Tail>,
    /// the checkpoint it began at is taken on trust
    fresh: bool,
}

impl Found {
    /// Reads the file up to byte `size`, from its first frame when `all`.
    fn up_to(
        source: &Source,
        layout: &Layout,
        (boot, tail): (Option<[u8; 16]>, Option<Tail>),
        size: u64,
        window: &(u64, Vec<u8>),
        all: bool,
    ) -> Result<Found, OpLogError> {
        let mut read = |at: u64, len: usize| held(source, window, at, len);
        let mut located = Located::empty();
        if !all {
            located = locate_past(layout, size, &mut read, &mut |len| source.stretches(0, len))?;
        }
        let (mut state, mut ops, mut stop) = pass(source, layout, &located, size, window)?;
        let tail = tail.filter(|tail| told_of(tail, &state, size, window));
        let by_boot = located.state.checkpoint != 0 && boot == Some(located.boot);
        let by_tail = tail.is_some_and(|tail| located.state.end <= tail.judged);
        let fresh = by_boot || by_tail;
        if !fresh && located.state.checkpoint != 0 && located.state.end > state.synced {
            // the machine went down since it was written: begin within what
            // is synced and read through all that follows
            let within = state.synced.max(FILE_HEADER_LEN as u64);
            located = locate_past(layout, within, &mut read, &mut |len| {
                source.stretches(0, len)
            })?;
            (state, ops, stop) = pass(source, layout, &located, size, window)?;
        }
        Ok(Found {
            located,
            state,
            ops,
            stop,
            tail,
            fresh,
        })
    }

    /// What an open holds of this read, of a file `seen` bytes long.
    fn loaded(self, hidden: Option<u64>, seen: u64, clamped: bool) -> Loaded {
        let (tail, fresh) = (self.tail, self.fresh);
        let synced = self.state.synced.max(tail.map_or(0, |tail| tail.synced));
        Loaded {
            closes: [0; 4],
            began: self.located.state.end,
            began_linked: self.located.state.linked_at,
            synced,
            judged: match tail {
                // ops somebody of this boot left to be looked at are still there
                Some(tail) if tail.pending => tail.judged.max(synced),
                _ if fresh => u64::MAX,
                _ => self.state.synced,
            },
            hidden,
            seen,
            clamped,
            passed: hidden.is_some(),
            state: self.state,
            ops: self.ops,
        }
    }
}

/// Reads the log `file` of `dir` as an open does; from its first frame
/// when `all`. The caller holds a lock on the file.
pub(crate) fn load(
    source: &Source,
    dir: &Path,
    layout: &Layout,
    boot: Option<[u8; 16]>,
    all: bool,
) -> Result<Loaded, OpLogError> {
    let meta = source.file.metadata()?;
    let mut size = crate::settle::read_end(&meta);
    let clamped = size < meta.len();
    let tail = Tail::read(dir, boot);
    // what a reader of this boot found to be no part of the log, or a
    // cut that was begun takes away
    let ends = |tail: &Tail| tail.hidden != 0 && tail.hidden == meta.len() && tail.ends < size;
    let hidden = tail.filter(ends);
    let hidden = hidden.map(|tail| {
        size = tail.ends;
        meta.len()
    });
    let size = size.max(FILE_HEADER_LEN as u64);
    // one read of the last block serves the search for its checkpoint and
    // the frames after it
    let from = match all {
        true => size,
        false => (size - 1) / layout.block * layout.block,
    };
    let window = (from, source.at(from, (size - from) as usize)?);
    let found = Found::up_to(source, layout, (boot, tail), size, &window, all)?;
    let stop = found.stop;
    let mut loaded = found.loaded(hidden, meta.len(), clamped);
    let mut read = |at: u64, len: usize| held(source, &window, at, len);
    if loaded.state.last != 0 {
        // the window may begin where that frame ends, when none after it reads
        let closes = read(loaded.state.end - 4, 4)?;
        loaded.closes.copy_from_slice(&closes);
    }
    let held = match stop {
        Stop::Unread => source.stretches(loaded.state.end, size)?,
        Stop::End => Vec::new(),
    };
    if stop == Stop::Unread && loaded.stopped(&mut read, layout, (size, &held))? {
        crate::settle::note_damage(&dir.join("durable"), loaded.state.end, size);
    }
    Ok(loaded)
}
