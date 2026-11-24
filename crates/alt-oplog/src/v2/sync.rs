//! Making what a handle appended durable, and recording how far.
//!
//! The boundary moves in one way only. A writer takes the end of its own
//! last frame: it read every frame before that under the lock it appended
//! under. What those frames name is synced first, by whoever the caller
//! gave for it, with no lock held. Then, under `lock`, the log is fsynced
//! and `tail` raised to that end and no further: a frame another writer
//! appended meanwhile lies past the boundary, whether or not the fsync
//! took it to the disk, because nobody synced what it names. The next
//! frame anyone appends carries the boundary into the log.
//!
//! No sync says of frames that were cut that they are synced, and none
//! tells of a cut to a caller whose frames are in the log. A sync
//! answers for the frames its caller holds (`owed`): the ones on the
//! ticket it is asked with, or, asked with none, the ones on no ticket.
//! Where one of them was cut it tells so (`CutBack`), once. A handle
//! that holds frames of other writers which are gone reads the log again
//! by itself and syncs what is there.

use crate::lock::{lock_exclusive, lock_shared, unlock};
use crate::v2::log::Log;
use crate::v2::owed::{Holder, Ticket};
use crate::v2::sink::{Reached, Sink, record};
use crate::v2::tail::{Held, Tail};
use crate::{OpLogError, SyncNamed, relaxed_durability};

/// The frames on no ticket.
fn of_the_handle(holder: Holder) -> bool {
    matches!(holder, Holder::Own | Holder::Left)
}

impl Log {
    pub fn sync(&mut self) -> Result<(), OpLogError> {
        // a handle that may not write the log appended nothing to it
        if relaxed_durability() || !self.writable {
            return Ok(());
        }
        if !self.defer {
            return self.sync_as(&of_the_handle, false);
        }
        // the sink fsyncs off the write path, records how far, and
        // answers for the frames left to it
        self.write_count += 1;
        let mut account = self.account.lock().unwrap();
        account.hand(Holder::Own, Holder::Left);
        drop(account);
        self.leave(&|holder| holder == Holder::Left)
    }

    /// A ticket for the frames this handle wrote that are on none and
    /// that nobody was answered for yet.
    pub fn ticket(&mut self) -> Ticket {
        self.account.lock().unwrap().ticket()
    }

    /// [`Self::sync`] for the holder of `ticket`.
    pub fn sync_for(&mut self, ticket: &Ticket) -> Result<(), OpLogError> {
        if relaxed_durability() || !self.writable {
            return Ok(());
        }
        let of = |holder| ticket.holds(holder);
        if !self.defer {
            return self.sync_as(&of, true);
        }
        self.write_count += 1;
        self.answered(&of, true)?;
        self.leave(&of)
    }

    /// Has a handle that may hold frames which are gone, or that stopped
    /// short of frames `of` is owed an answer for, read the log to its
    /// end as a writer does.
    fn resolved(&mut self, of: &dyn Fn(Holder) -> bool) -> Result<(), OpLogError> {
        let account = self.account.lock().unwrap();
        let behind = account.stale || (self.hidden.is_some() && account.owes(of));
        drop(account);
        if !behind {
            return Ok(());
        }
        self.locked(|log| log.read_to_end().and_then(|()| log.owed_looked()))??;
        self.account.lock().unwrap().stale = false;
        // a cut the header tells of that reading the log to its end
        // neither found made nor took back is none this handle sets a
        // boundary under
        let told = self.told()?;
        match told.cuts(self.state.generation, self.synced, self.state.end) {
            true => Err(OpLogError::CutBack),
            false => Ok(()),
        }
    }

    /// Tells `of`, once, where frames it holds were found cut: of every
    /// one that is gone by now, which are looked for first.
    fn answered(&mut self, of: &dyn Fn(Holder) -> bool, spent: bool) -> Result<(), OpLogError> {
        if self.account.lock().unwrap().found_cut_of(of) {
            lock_shared(&self.file)?;
            let looked = self.owed_looked();
            let _ = unlock(&self.file);
            looked?;
        }
        self.account.lock().unwrap().answer(of, spent).map(drop)
    }

    /// Syncs what this handle holds and answers for the frames `of`
    /// holds: where one was cut, `of` is told so, with nothing of it
    /// owed an answer after that when `spent`.
    fn sync_as(&mut self, of: &dyn Fn(Holder) -> bool, spent: bool) -> Result<(), OpLogError> {
        loop {
            self.resolved(of)?;
            self.answered(of, spent)?;
            match self.sync_to_end() {
                // frames it holds are gone: none `of` holds, as far as
                // it has looked
                Err(OpLogError::CutBack) => {}
                Err(other) => return Err(other),
                Ok(()) if !self.owes_past(of) => return Ok(()),
                // frames `of` holds lie past the ones it holds
                Ok(()) => {}
            }
            self.account.lock().unwrap().stale = true;
        }
    }

    fn owes_past(&self, of: &dyn Fn(Holder) -> bool) -> bool {
        let account = self.account.lock().unwrap();
        account.owes_past(of, self.state.end)
    }

    /// Leaves what this handle holds to its sink, which answers for the
    /// frames `of` holds.
    fn leave(&mut self, of: &dyn Fn(Holder) -> bool) -> Result<(), OpLogError> {
        loop {
            self.resolved(of)?;
            match self.defer_sync() {
                Err(OpLogError::CutBack) => {}
                Err(other) => return Err(other),
                Ok(()) if !self.owes_past(of) => return Ok(()),
                Ok(()) => {}
            }
            self.account.lock().unwrap().stale = true;
        }
    }

    pub fn set_defer_durability(&mut self, on: bool) {
        self.defer = on;
    }

    pub fn write_count(&self) -> u64 {
        self.write_count
    }

    pub fn syncs_inline(&self) -> bool {
        !self.defer && !relaxed_durability()
    }

    pub fn sync_named_with(&mut self, sync: SyncNamed) {
        self.named = Some(sync);
    }

    pub fn sync_handle(&self) -> Result<Sink, OpLogError> {
        Ok(Sink {
            file: self.file.try_clone()?,
            dir: self.dir.clone(),
            boot: self.boot,
            id: self.layout.id,
            account: self.account.clone(),
            named: self.named.clone(),
        })
    }

    pub fn bound_now(&mut self) -> Result<(), OpLogError> {
        lock_exclusive(&self.file)?;
        let bounded = self.bounded_to_end();
        let _ = unlock(&self.file);
        bounded?;
        if relaxed_durability() {
            return Ok(());
        }
        // a sink answers for what was left to it
        let of: fn(Holder) -> bool = match self.defer {
            true => |holder| holder == Holder::Own,
            false => of_the_handle,
        };
        self.sync_as(&of, false)
    }

    /// With the lock held: what a writer does before it appends, by a
    /// handle that reads the log again where frames it holds are gone.
    fn bounded_to_end(&mut self) -> Result<(), OpLogError> {
        // each time ops it holds are cut while it has given up the lock
        // for whoever syncs what ops name
        while let Err(told) = self.catch_up_bounded() {
            match told {
                OpLogError::CutBack => self.read_to_end()?,
                other => return Err(other),
            }
        }
        // over a frame this build would have to know it sets no boundary
        let noted = self.tail();
        if self.said_synced(noted) < self.state.end && !self.knows_all()? {
            self.may_append()?;
        }
        Ok(())
    }

    /// Up to where a sync of this handle records the log as synced, with
    /// the four bytes before that: the last frame it holds. Of a log with
    /// a frame this build would have to know, it is the last frame the
    /// handle wrote itself that no sync is recorded over: what lies past
    /// it may be named by the frame it does not know.
    pub(super) fn sync_target(&self) -> Result<Option<(u64, [u8; 4])>, OpLogError> {
        match self.knows_all_unlocked()? {
            true => Ok(Some((self.state.end, self.closes))),
            false => Ok(self.account.lock().unwrap().last_by(self.state.end)),
        }
    }

    /// Records the log as synced up to the last frame this handle holds,
    /// fsyncing it unless another committer's fsync already covered it.
    /// `CutBack` where frames it holds are gone.
    fn sync_to_end(&mut self) -> Result<(), OpLogError> {
        let Some((target, closes)) = self.sync_target()? else {
            return Ok(());
        };
        let noted = self.tail();
        if self.said_synced(noted) >= target {
            self.unsynced = false;
            self.account.lock().unwrap().covered(target);
            return Ok(());
        }
        self.may_write()?;
        self.looked_to(target)?;
        if let Some(sync) = &self.named {
            sync(&self.named_unlocked(self.synced)?)?;
        }
        let held = Held::take(&self.dir)?;
        let noted = Tail::noted_held(&self.beside(), &held);
        self.synced = self.synced.max(self.said_synced(noted.says()));
        // frames up to there were cut since the append, or the log tells
        // of a cut that takes them: no boundary is set over them
        let generation = self.state.generation;
        let cut = self.told()?.cuts(generation, self.synced, target);
        if cut || !self.holds_head(self.file.metadata()?.len())? {
            return Err(OpLogError::CutBack);
        }
        record(
            &self.beside(),
            &held,
            noted,
            &Reached::at(target, closes, generation),
        )?;
        self.unsynced = false;
        self.synced = self.synced.max(target);
        self.account.lock().unwrap().covered(target);
        Ok(())
    }
}
