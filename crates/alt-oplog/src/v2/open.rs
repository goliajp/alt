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

use std::cell::Cell;
use std::fs::File;
use std::path::Path;

use crate::read::read_at;
use crate::scan::parse_body;
use crate::v2::block::{BLOCK_HEADER_LEN, FILE_HEADER_LEN, Layout};
use crate::v2::frame::{CLOSE_LEN, HEADER_LEN, KIND_OP, View};
use crate::v2::scan::{Located, Stop, Walk, checkpoint_at, locate, read_frame};
use crate::v2::state::State;
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
        inverse_of: view.ext.inverse_of,
        request: view.ext.request,
    });
    Ok(op)
}

/// Bytes of the log file as an open reads them, counted.
pub(crate) struct Source<'a> {
    pub file: &'a File,
    pub read: &'a Cell<u64>,
}

impl Source<'_> {
    pub fn at(&self, at: u64, len: usize) -> std::io::Result<Vec<u8>> {
        self.read.set(self.read.get() + len as u64);
        read_at(self.file, at, len)
    }
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
    pub clamped: bool,
    pub passed: bool,
}

/// The frames that read in bytes of a log that follow no frame: `rest`,
/// which begin at byte `base` of the file.
pub(crate) struct Salvaged {
    pub ops: Vec<Op>,
    /// how many of the bytes their frames take up
    pub covered: u64,
    /// the furthest one of them says the log was synced
    pub claim: u64,
    /// where the first of them starts
    pub first: Option<u64>,
}

fn earlier(one: Option<u64>, other: Option<u64>) -> Option<u64> {
    match (one, other) {
        (Some(one), Some(other)) => Some(one.min(other)),
        (one, other) => one.or(other),
    }
}

impl Salvaged {
    fn note(&mut self, at: u64, end: u64, view: &View) {
        if view.head.kind == KIND_OP
            && let Ok(op) = op_of(at, end, view)
        {
            self.ops.push(op);
        }
        self.covered += end - at;
        self.claim = self.claim.max(view.head.synced);
        self.first = earlier(self.first, Some(at));
    }
}

/// Looks for frames in `rest`: from every block in it that opens with a
/// checkpoint, and back from the end of it through the last block.
pub(crate) fn salvage(layout: &Layout, rest: &[u8], base: u64, generation: u32) -> Salvaged {
    let mut found = Salvaged {
        ops: Vec::new(),
        covered: 0,
        claim: 0,
        first: None,
    };
    let limit = base + rest.len() as u64;
    let mut read = |at: u64, len: usize| {
        let from = at.saturating_sub(base) as usize;
        Ok(rest.get(from..from + len).unwrap_or_default().to_vec())
    };
    let mut block = base.next_multiple_of(layout.block);
    let mut walked_to = base;
    while block + (BLOCK_HEADER_LEN as u64) < limit {
        let header = layout.decode_header(&rest[(block - base) as usize..], block);
        let opened = match header {
            Some(header) if header.first as usize == BLOCK_HEADER_LEN => {
                checkpoint_at(layout, limit, block + BLOCK_HEADER_LEN as u64, &mut read)
            }
            _ => Ok(None),
        };
        block += layout.block;
        let Ok(Some(Located { mut state, .. })) = opened else {
            continue;
        };
        found.covered += state.end - state.last;
        found.claim = found.claim.max(state.synced);
        found.first = earlier(found.first, Some(state.last));
        let walk = Walk {
            layout: *layout,
            data: rest,
            base,
            strict: false,
        };
        let _ = walk.run(&mut state, &mut |at, end, view| {
            found.note(at, end, view);
            Ok(())
        });
        walked_to = state.end;
        block = block.max(state.end.next_multiple_of(layout.block));
    }
    // the frames that lie one after another up to the end of the file
    let mut end = limit;
    let mut last = Vec::new();
    while walked_to < limit && end >= walked_to + (HEADER_LEN + CLOSE_LEN) as u64 {
        let close = &rest[(end - base) as usize - CLOSE_LEN..][..4];
        let len = u32::from_le_bytes(close.try_into().unwrap()) as u64;
        let Some(at) = end.checked_sub(len).filter(|at| *at >= walked_to) else {
            break;
        };
        if at / layout.block != (end - 1) / layout.block {
            break;
        }
        let bytes = &rest[(at - base) as usize..(end - base) as usize];
        let Some(view) = read_frame(layout, bytes, generation) else {
            break;
        };
        let mut one = Salvaged {
            ops: Vec::new(),
            covered: 0,
            claim: 0,
            first: None,
        };
        one.note(at, end, &view);
        last.push(one);
        end = at;
    }
    for one in last.into_iter().rev() {
        found.ops.extend(one.ops);
        found.covered += one.covered;
        found.claim = found.claim.max(one.claim);
        found.first = earlier(found.first, one.first);
    }
    found
}

/// Reads on from `located` to byte `size` of the file.
fn pass(
    source: &Source,
    layout: &Layout,
    located: &Located,
    size: u64,
    window: &mut (u64, Vec<u8>),
) -> Result<(State, Vec<Op>, Stop), OpLogError> {
    let mut state = located.state.clone();
    if state.end < window.0 {
        *window = (
            state.end,
            source.at(state.end, (size - state.end) as usize)?,
        );
    }
    let walk = Walk {
        layout: *layout,
        data: &window.1,
        base: window.0,
        strict: false,
    };
    let mut ops = Vec::new();
    let stop = walk.run(&mut state, &mut |at, end, view| {
        if view.head.kind == KIND_OP {
            ops.push(op_of(at, end, view)?);
        }
        Ok(())
    })?;
    Ok((state, ops, stop))
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
    // what a reader of this boot found to be no part of the log
    let mut hidden = None;
    if let Some(tail) = &tail
        && tail.hidden == meta.len()
        && tail.judged < size
    {
        (size, hidden) = (tail.judged, Some(meta.len()));
    }
    let size = size.max(FILE_HEADER_LEN as u64);
    // one read of the last block serves the search for its checkpoint and
    // the frames after it
    let from = match all {
        true => 0,
        false => (size - 1) / layout.block * layout.block,
    };
    let mut window = (from, source.at(from, (size - from) as usize)?);
    let mut located = Located::empty();
    if !all {
        let mut read = |at: u64, len: usize| match at.checked_sub(window.0) {
            Some(skip) if skip as usize + len <= window.1.len() => {
                Ok(window.1[skip as usize..skip as usize + len].to_vec())
            }
            _ => source.at(at, len),
        };
        located = locate(layout, size, &mut read)?;
    }
    let (mut state, mut ops, mut stop) = pass(source, layout, &located, size, &mut window)?;
    let tail = tail.filter(|tail| {
        // the bytes before its boundary, where this read holds them
        let closes = tail.synced.checked_sub(4 + window.0).map(|at| at as usize);
        let held = closes.and_then(|at| window.1.get(at..at + 4));
        tail.generation == state.generation
            && tail.judged <= size
            && held.is_none_or(|held| held == tail.closes || tail.closes == [0; 4])
    });
    let by_boot = located.state.checkpoint != 0 && boot == Some(located.boot);
    let by_tail = tail.is_some_and(|tail| located.state.end <= tail.judged);
    let fresh = by_boot || by_tail;
    if !fresh && located.state.checkpoint != 0 && located.state.end > state.synced {
        // the machine went down since it was written: begin within what
        // is synced and read through all that follows
        let mut read = |at: u64, len: usize| source.at(at, len);
        located = locate(layout, state.synced.max(FILE_HEADER_LEN as u64), &mut read)?;
        (state, ops, stop) = pass(source, layout, &located, size, &mut window)?;
    }
    let mut loaded = Loaded {
        closes: [0; 4],
        began: located.state.end,
        began_linked: located.state.linked_at,
        synced: state.synced.max(tail.map_or(0, |tail| tail.synced)),
        judged: match tail {
            // what a reader of this boot would not vouch for is still there
            Some(tail) if tail.hidden != 0 => tail.judged,
            _ if fresh => u64::MAX,
            _ => state.synced,
        },
        hidden,
        clamped,
        passed: hidden.is_some(),
        state,
        ops,
    };
    if loaded.state.last != 0 {
        let at = (loaded.state.end - 4 - window.0) as usize;
        loaded.closes.copy_from_slice(&window.1[at..at + 4]);
    }
    if stop == Stop::Unread {
        let rest = &window.1[(loaded.state.end - window.0) as usize..];
        let found = salvage(layout, rest, loaded.state.end, loaded.state.generation);
        if loaded.state.end < loaded.synced.max(found.claim) {
            if !crate::settle::reading_past_damage() {
                return Err(OpLogError::Format("oplog record corrupt"));
            }
            crate::settle::note_damage(&dir.join("durable"), loaded.state.end, size);
            loaded.clamped = true;
        } else {
            loaded.passed = true;
        }
    }
    Ok(loaded)
}
