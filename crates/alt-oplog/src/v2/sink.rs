//! The fsync a daemon makes of the log off the write path, through a
//! handle of its own, and how far a log handle knows its appends synced.
//!
//! The handle leaves for its sink up to where to sync, and the two share
//! the account of the frames the handle wrote (`owed`). An fsync of the
//! sink records the log as synced up to there and no frame before it is
//! owed an answer any longer, or it finds the frames up to there out of
//! their place: then it looks at each frame owed, and the ones that are
//! gone are told of to whoever holds them, by the sink or by the handle,
//! whichever that one asks first.

use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use crate::v2::log::Log;
use crate::v2::mark::Mark;
use crate::v2::owed::{Answer, Holder, Owed, Shared, Ticket};
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
}

/// Whether the frame that ends at byte `end` of `file` with `closes`,
/// written to a log of `generation`, is where it was written: `None`
/// where the bytes are there and the header tells `told` of a cut that
/// takes it, which may or may not have been made.
fn placed(
    file: &File,
    told: Mark,
    synced: u64,
    (end, closes, generation): (u64, [u8; 4], u32),
) -> std::io::Result<Option<bool>> {
    if file.metadata()?.len() < end {
        return Ok(Some(false));
    }
    let mut held = [0u8; 4];
    match file.read_exact_at(&mut held, end - 4) {
        Ok(()) if held != closes => Ok(Some(false)),
        Ok(()) if told.cuts(generation, synced, end) => Ok(None),
        Ok(()) => Ok(Some(true)),
        Err(short) if short.kind() == ErrorKind::UnexpectedEof => Ok(Some(false)),
        Err(other) => Err(other),
    }
}

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

    fn same(&self, other: &Reached) -> bool {
        (self.target, self.closes, self.generation)
            == (other.target, other.closes, other.generation)
    }

    /// Whether the frame it ends with is where it was in `file`, whose
    /// header tells `told` of a cut: no cut took it, and none is told
    /// that takes it.
    fn in_place(&self, file: &File, told: Mark) -> std::io::Result<bool> {
        let frame = (self.target, self.closes, self.generation);
        Ok(placed(file, told, self.synced, frame)? == Some(true))
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
        crate::v2::fsync(of.log)?;
        tail.generation = tail.generation.max(reached.generation);
        tail.raise(reached.target, Some(reached.closes));
        tail.judged = tail.judged.max(reached.target);
        tail.write(of, held)?;
    }
    Ok(())
}

impl Log {
    /// Leaves what this handle holds for its sink to sync and to record.
    pub(super) fn defer_sync(&mut self) -> Result<(), OpLogError> {
        let Some((target, closes)) = self.sync_target()? else {
            return Ok(());
        };
        // without a frame there is nothing whose place a sink could
        // find changed
        if self.state.last == 0 {
            return Ok(());
        }
        let reached = Reached::at(target, closes, self.state.generation);
        if self.account.lock().unwrap().left.same(&reached) {
            return Ok(());
        }
        let synced = self.noted_bound();
        if synced < target {
            self.looked_to(target)?;
        }
        let pending = self.named_unlocked(synced.max(self.synced_here()))?;
        let mut account = self.account.lock().unwrap();
        account.synced = account.synced.max(self.synced);
        account.left = Reached {
            pending,
            synced: account.synced,
            ..reached
        };
        Ok(())
    }

    pub(super) fn note_synced(&self, synced: u64) {
        let mut account = self.account.lock().unwrap();
        account.synced = account.synced.max(synced);
    }

    pub fn synced_here(&self) -> u64 {
        self.account.lock().unwrap().synced
    }
}

pub(crate) struct Sink {
    pub(super) file: File,
    pub(super) dir: PathBuf,
    pub(super) boot: Option<[u8; 16]>,
    pub(super) id: [u8; 16],
    pub(super) account: Shared,
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
        let reached = self.account.lock().unwrap().left.clone();
        // what was cut names nothing to see to: the fsync finds it gone
        if reached.target != 0
            && !self.covered(&reached)
            && let Some(sync) = &self.named
            && reached.in_place(&self.file, Mark::read_unlocked(&self.file)?)?
        {
            sync(&reached.pending)?;
        }
        Ok(reached)
    }

    /// Fsyncs the log and records it as synced up to where `reached`
    /// ends, where the frames up to there are in their place. Where
    /// they are not, each frame owed is looked at, and the handle is
    /// left to read the log again.
    fn sync_to(&self, mut reached: Reached) -> Result<(), OpLogError> {
        if reached.target == 0 {
            crate::v2::fsync(&self.file)?;
            return Ok(());
        }
        let of = self.beside();
        let held = Held::take(&self.dir)?;
        let noted = Tail::noted_held(&of, &held);
        let said = noted
            .says()
            .map_or(0, |tail| tail.synced_of(reached.generation));
        reached.synced = reached.synced.max(said);
        let told = Mark::read(&self.file)?;
        if !reached.in_place(&self.file, told)? {
            let mut account = self.account.lock().unwrap();
            if account.left.same(&reached) {
                account.left = Reached::default();
            }
            account.stale = true;
            let synced = reached.synced.max(account.synced);
            let frame = |owed: &Owed| (owed.end, owed.closes, owed.generation);
            account.look(|owed| placed(&self.file, told, synced, frame(owed)))?;
            return Ok(());
        }
        // with nobody to see to what ops name, no sync is recorded over
        // an op that names something
        if self.named.is_none() && said < reached.target && !reached.pending.is_empty() {
            crate::v2::fsync(&self.file)?;
            self.account.lock().unwrap().fsynced(reached.target);
            return Ok(());
        }
        record(&of, &held, noted, &reached)?;
        self.account.lock().unwrap().covered(reached.target);
        Ok(())
    }

    /// Answers whoever asks with no ticket, for the frames the handle
    /// left to this sink: the caller of one handle, which is all it
    /// knows of them. Told of a cut, that caller is answered for none of
    /// them any longer.
    pub fn fsync_to(&self, reached: Reached) -> Result<(), OpLogError> {
        self.sync_to(reached)?;
        let mut account = self.account.lock().unwrap();
        let of = |holder| holder == Holder::Left;
        match account.answer(&of, true)? {
            // some are in the log with no sync recorded over them, and
            // nothing is left to sync them up to: of whose they are it
            // cannot tell, and tells of the cut that left it so
            Answer::Owed if account.left.target == 0 => {
                account.spend(&of);
                Err(OpLogError::CutBack)
            }
            Answer::Owed | Answer::Synced => Ok(()),
        }
    }

    /// What became of the frames on `ticket`, as far as is known.
    pub fn answer(&self, ticket: &Ticket) -> Result<Answer, OpLogError> {
        let mut account = self.account.lock().unwrap();
        account.answer(&|holder| ticket.holds(holder), true)
    }

    /// Fsyncs for the holder of `ticket`, where no sync is recorded
    /// over its frames yet.
    pub fn fsync_for(&self, ticket: &Ticket) -> Result<Answer, OpLogError> {
        if self.answer(ticket)? == Answer::Synced {
            return Ok(Answer::Synced);
        }
        self.sync_to(self.reached()?)?;
        self.answer(ticket)
    }
}
