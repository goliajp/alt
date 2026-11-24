//! The fsync a daemon makes of the log off the write path, through a
//! handle of its own, and how far a log handle knows its appends synced.
//!
//! What a handle leaves for its sink to sync comes to one of two ends:
//! the sink records it as synced, or the sink tells whoever waits for it,
//! once, that its frames were cut (`CutBack`). It is neither dropped nor
//! laid over: a handle that reads the log anew leaves it where it is, and
//! one that asks again while what it left before is cut is told so, with
//! nothing new left to the sink until the sink has told.

use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::v2::log::Log;
use crate::v2::mark::Mark;
use crate::v2::tail::{Held, Noted, Place, Tail};
use crate::{Op, OpLogError, SyncNamed};

/// What a handle's appends reached when it last asked for them to be
/// synced, for its sink to record.
#[derive(Debug, Clone, Default)]
pub struct Reached {
    /// where its last frame ends, zero when there is nothing to sync
    target: u64,
    /// the four bytes of the file before that
    closes: [u8; 4],
    generation: u32,
    /// the ops up to there that nobody recorded as synced
    pending: Vec<Op>,
    /// the furthest boundary the handle or its sink saw recorded
    synced: u64,
    /// the sink has neither recorded this as synced nor told of its cut
    owed: bool,
    /// how often the handle had read the log anew when it left this
    reads: u64,
}

pub(crate) type Deferred = Arc<Mutex<Reached>>;

impl Reached {
    /// What a sync that the handle makes itself reaches.
    pub(super) fn at(target: u64, closes: [u8; 4], generation: u32) -> Reached {
        Reached {
            target,
            closes,
            generation,
            ..Reached::default()
        }
    }

    /// Whether it is owed an answer for and ends at `target` or beyond.
    fn covers(&self, target: u64) -> bool {
        self.owed && self.target >= target
    }

    fn same(&self, other: &Reached) -> bool {
        (self.target, self.closes, self.generation)
            == (other.target, other.closes, other.generation)
    }

    /// Whether the frame it ends with is where it was in `file`, whose
    /// header tells `told` of a cut: no cut took it, and none is told
    /// that takes it.
    fn in_place(&self, file: &File, told: Mark) -> std::io::Result<bool> {
        if told.cuts(self.generation, self.synced, self.target) {
            return Ok(false);
        }
        if file.metadata()?.len() < self.target {
            return Ok(false);
        }
        let mut closes = [0u8; 4];
        match file.read_exact_at(&mut closes, self.target - 4) {
            Ok(()) => Ok(closes == self.closes),
            Err(short) if short.kind() == ErrorKind::UnexpectedEof => Ok(false),
            Err(other) => Err(other),
        }
    }
}

/// The last step of a sync, for whoever holds `lock` and found the frames
/// up to where `reached` ends in their place: fsyncs the log and notes it
/// as synced up to there, where `noted`, which the note beside it says,
/// does not say so already.
pub(super) fn record(
    of: &Place,
    held: &Held,
    noted: Noted,
    reached: &Reached,
) -> std::io::Result<()> {
    let mut tail = noted.to_sync(reached.generation);
    if tail.synced < reached.target {
        of.log.sync_all()?;
        tail.generation = tail.generation.max(reached.generation);
        tail.raise(reached.target, Some(reached.closes));
        tail.judged = tail.judged.max(reached.target);
        tail.write(of, held)?;
    }
    Ok(())
}

impl Log {
    /// Leaves what this handle appended so far for its sink to sync and
    /// to record. What it left before and the sink still owes an answer
    /// for stays as it is while frames of it are cut: the handle is told
    /// so, and the sink tells whoever waits for it.
    pub(super) fn defer_sync(&mut self) -> Result<(), OpLogError> {
        let Some((target, closes)) = self.sync_target()? else {
            return Ok(());
        };
        // without a frame there is nothing whose place a sink could
        // find changed
        if self.state.last == 0 {
            return Ok(());
        }
        if self.left_cut(target)? {
            return Err(OpLogError::CutBack);
        }
        // the frames this handle wrote are the sink's to answer for
        if self.deferred.lock().unwrap().covers(target) {
            self.answered_to(target);
            return Ok(());
        }
        let synced = self.noted_bound();
        let pending = self.named_unlocked(synced.max(self.synced_here()))?;
        {
            let mut reached = self.deferred.lock().unwrap();
            (reached.target, reached.closes) = (target, closes);
            reached.generation = self.state.generation;
            reached.pending = pending;
            reached.synced = reached.synced.max(self.synced);
            (reached.owed, reached.reads) = (true, self.reads);
        }
        self.answered_to(target);
        Ok(())
    }

    /// Whether frames this handle left for a sink, which gave no answer
    /// for them yet, are cut, where a sync up to `target` does not find
    /// that out by itself: a handle that has not read the log anew since
    /// holds what it left before among the frames it syncs now.
    pub(super) fn left_cut(&self, target: u64) -> Result<bool, OpLogError> {
        let left = self.deferred.lock().unwrap().clone();
        let since = (left.generation, left.reads) != (self.state.generation, self.reads);
        if !left.owed || !(since || left.target > target) {
            return Ok(false);
        }
        Ok(!left.in_place(&self.file, Mark::read_unlocked(&self.file)?)?)
    }

    /// What this handle left for a sink is synced up to `target` by the
    /// handle itself: the sink owes no answer for it.
    pub(super) fn left_synced(&self, target: u64) {
        let mut reached = self.deferred.lock().unwrap();
        reached.synced = reached.synced.max(target);
        if reached.target <= target {
            reached.owed = false;
        }
    }

    /// Tells of the cut of what this handle left for a sink in the
    /// sink's place, for a handle that syncs by itself: the sink has
    /// nothing left to tell of.
    pub(super) fn left_told(&self) {
        let mut reached = self.deferred.lock().unwrap();
        (reached.target, reached.pending, reached.owed) = (0, Vec::new(), false);
    }

    pub(super) fn note_synced(&self, synced: u64) {
        let mut reached = self.deferred.lock().unwrap();
        reached.synced = reached.synced.max(synced);
    }

    pub fn synced_here(&self) -> u64 {
        self.deferred.lock().unwrap().synced
    }
}

pub(crate) struct Sink {
    pub(super) file: File,
    pub(super) dir: PathBuf,
    pub(super) boot: Option<[u8; 16]>,
    pub(super) id: [u8; 16],
    pub(super) appended: Deferred,
    pub(super) named: Option<SyncNamed>,
}

impl Sink {
    fn beside(&self) -> Place<'_> {
        Place {
            dir: &self.dir,
            boot: self.boot,
            id: self.id,
            log: &self.file,
        }
    }

    fn covered(&self, reached: &Reached) -> bool {
        let noted = Tail::read(&self.beside());
        noted.is_some_and(|tail| tail.synced_of(reached.generation) >= reached.target)
    }

    pub fn reached(&self) -> Result<Reached, OpLogError> {
        let reached = self.appended.lock().unwrap().clone();
        // what was cut names nothing to see to: the fsync tells of it
        if reached.target != 0
            && !self.covered(&reached)
            && let Some(sync) = &self.named
            && reached.in_place(&self.file, Mark::read_unlocked(&self.file)?)?
        {
            sync(&reached.pending)?;
        }
        Ok(reached)
    }

    pub fn fsync_to(&self, mut reached: Reached) -> Result<(), OpLogError> {
        if reached.target == 0 {
            self.file.sync_all()?;
            return Ok(());
        }
        let of = self.beside();
        let held = Held::take(&self.dir)?;
        let noted = Tail::noted_held(&of, &held);
        let said = noted
            .says()
            .map_or(0, |tail| tail.synced_of(reached.generation));
        reached.synced = reached.synced.max(said);
        if !reached.in_place(&self.file, Mark::read(&self.file)?)? {
            // the frames were cut: whoever waits for this is told, and
            // there is nothing of this handle's to sync until it asks
            // again
            let mut appended = self.appended.lock().unwrap();
            if appended.same(&reached) {
                (appended.target, appended.pending) = (0, Vec::new());
                appended.owed = false;
            }
            return Err(OpLogError::CutBack);
        }
        record(&of, &held, noted, &reached)?;
        let mut appended = self.appended.lock().unwrap();
        appended.synced = appended.synced.max(reached.target);
        if appended.same(&reached) {
            appended.owed = false;
        }
        Ok(())
    }
}
