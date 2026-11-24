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

use crate::scan::parse_body;
use crate::v2::block::{FILE_HEADER_LEN, Layout};
use crate::v2::frame::{KIND_OP, View};
use crate::v2::locate::locate_past;
use crate::v2::mark::Mark;
use crate::v2::salvage::salvage;
use crate::v2::scan::{Located, Stop, Walk};
pub(crate) use crate::v2::source::Source;
use crate::v2::state::State;
use crate::v2::stream::{ReadAt, walk_file};
use crate::v2::tail::{Noted, Place, Tail};
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
        staged: view.ext.index_after.is_some(),
        retired: view.ext.retired,
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

/// Whether what `tail` says is said of the log as this read found it, in
/// a file `len` bytes long.
fn told_of(tail: &Tail, state: &State, len: u64, window: &(u64, Vec<u8>)) -> bool {
    // the bytes before its boundary, where this read holds them
    let closes = tail
        .synced
        .checked_sub(4)
        .and_then(|at| at.checked_sub(window.0));
    let held = closes.and_then(|at| window.1.get(at as usize..)?.first_chunk::<4>());
    tail.generation == state.generation
        && tail.judged <= len
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
    /// `tail` does not read, is gone, or speaks of another log: what it
    /// said of the ops left to look at is not known
    unknown: bool,
    /// the bytes of the file it holds, from the byte it names on
    window: (u64, Vec<u8>),
}

impl Found {
    /// Reads the file, which is `len` bytes long, up to byte `size`; from
    /// its first frame when `all`.
    fn up_to(
        source: &Source,
        layout: &Layout,
        (boot, noted): (Option<[u8; 16]>, Noted),
        (size, len): (u64, u64),
        all: bool,
    ) -> Result<Found, OpLogError> {
        let said = noted.says();
        // one read of the last block serves the search for its checkpoint
        // and the frames after it
        let from = match all {
            true => size,
            false => (size - 1) / layout.block * layout.block,
        };
        let window = (from, source.at(from, (size - from) as usize)?);
        let mut read = |at: u64, len: usize| held(source, &window, at, len);
        let mut located = Located::empty();
        if !all {
            located = locate_past(layout, size, &mut read, &mut |len| source.stretches(0, len))?;
        }
        let (mut state, mut ops, mut stop) = pass(source, layout, &located, size, &window)?;
        let tail = said.filter(|tail| told_of(tail, &state, len, &window));
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
            (state, ops, stop) = pass(source, layout, &located, size, &window)?;
        }
        Ok(Found {
            located,
            state,
            ops,
            stop,
            tail,
            fresh,
            unknown: matches!(noted, Noted::Unread | Noted::Gone)
                || said.is_some() != tail.is_some(),
            window,
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
                // a note that does not read or is gone may have said the
                // same, and so may the note of a log that was cut since
                _ if fresh && !self.unknown => u64::MAX,
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

/// Reads the log `of` as an open does; from its first frame when `all`.
/// The caller holds a lock on the file, and `told` is what its header
/// tells of a cut.
pub(crate) fn load(
    source: &Source,
    of: &Place,
    layout: &Layout,
    told: Mark,
    all: bool,
) -> Result<Loaded, OpLogError> {
    let meta = source.file.metadata()?;
    let size = crate::settle::read_end(&meta);
    let clamped = size < meta.len();
    let size = size.max(FILE_HEADER_LEN as u64);
    let noted = Tail::noted(of);
    let tail = noted.says();
    let lens = (size, meta.len());
    let mut found = Found::up_to(source, layout, (of.boot, noted), lens, all)?;
    // to whoever opens a file of the length a reader of this boot noted,
    // the log ends where that says, while the log in the file is of the
    // generation it was said of: a file that was cut since and grew back
    // to the length holds another log. And the log ends where its header
    // tells of a cut that this read did not find made: the read may have
    // begun past it, at a checkpoint the cut left behind
    let generation = found.state.generation;
    let ends = |tail: &Tail| tail.hides(meta.len(), generation);
    let noted_end = tail.filter(ends).map(|tail| tail.ends);
    // the boundary a handle reads on with: the note's with the frames'
    let synced = found.tail.map_or(0, |tail| tail.synced);
    let begun = told.begun_at(generation, found.state.synced.max(synced));
    let hidden = [noted_end, begun].into_iter().flatten().min();
    let hidden = hidden.filter(|ends| *ends < size);
    let size = hidden.map_or(size, |ends| ends.max(FILE_HEADER_LEN as u64));
    if hidden.is_some() {
        found = Found::up_to(source, layout, (of.boot, noted), (size, meta.len()), all)?;
    }
    let (stop, window) = (found.stop, std::mem::take(&mut found.window));
    let mut loaded = found.loaded(hidden.map(|_| meta.len()), meta.len(), clamped);
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
        crate::settle::note_damage(&of.dir.join("durable"), loaded.state.end, size);
    }
    Ok(loaded)
}
