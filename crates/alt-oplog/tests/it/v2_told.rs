//! What a caller of a driven sequence that asks without a ticket knows
//! of its frames beside the answers of syncs: a handle read the log anew
//! with that caller told that ops were cut, or cut ops itself.

use std::sync::atomic::Ordering;

use super::v2_driver::Run;
use super::v2_owed::Wrote;

impl Run {
    /// Handle `index` read the log anew with its caller told that ops
    /// were cut: of its frames on no ticket that are gone, that caller
    /// knows, and of the ones past where the log ends for the handle
    /// now, at a cut that was begun. Those are owed a sync still while
    /// the log holds them, and are told of to nobody once it does not.
    pub fn forget_gone(&mut self, index: usize) {
        let end = self.handles[index].log.appended_len();
        let mine = std::mem::take(&mut self.handles[index].mine);
        let mine: Vec<Wrote> = mine.into_iter().filter(|frame| self.has(frame)).collect();
        let past = mine.iter().filter(|frame| frame.end() > end).copied();
        let handle = &mut self.handles[index];
        handle.told.extend(past);
        handle.mine = mine;
    }

    /// Of `frames`, which handle `index` wrote on no ticket, the ones
    /// their caller is answered for: not those it was told of before
    /// that are gone by now.
    pub fn answered_for(&self, index: usize, frames: Vec<Wrote>) -> Vec<Wrote> {
        let told = &self.handles[index].told;
        let owed = |frame: &Wrote| self.has(frame) || !told.contains(frame);
        frames.into_iter().filter(owed).collect()
    }

    /// How many of the frames of handle `index` on no ticket the log
    /// holds, from the first on.
    pub fn in_the_log(&self, index: usize) -> Vec<bool> {
        let mine = self.handles[index].mine.iter();
        mine.map(|frame| self.has(frame)).collect()
    }

    /// Handle `index` cut the log itself: of its frames on no ticket
    /// that the log held before, as `had` says, and holds no longer,
    /// its caller is told by the look that cut them.
    pub fn forget_cut_now(&mut self, index: usize, had: Vec<bool>) {
        let mine = std::mem::take(&mut self.handles[index].mine);
        let kept = mine.into_iter().zip(had);
        let kept = kept.filter(|(frame, had)| !had || self.has(frame));
        self.handles[index].mine = kept.map(|(frame, _)| frame).collect();
    }

    /// A writer is told to leave what is written unsynced and syncs or
    /// sets a boundary, for its caller without a ticket or for one
    /// with: nothing is promised, and that caller is owed no answer
    /// after for the frames that were written so told. For what a
    /// writer that syncs wrote it is owed one still.
    pub fn sync_unpromised(&mut self) {
        let index = self.writer();
        self.sync_unpromised_of(index);
    }

    /// [`Self::sync_unpromised`] by handle `index`.
    pub fn sync_unpromised_of(&mut self, index: usize) {
        let before = (self.handles[index].log.reads(), self.file_len());
        let handle = &mut self.handles[index];
        let claim = (!handle.claims.is_empty() && self.also.one_in(2)).then_some(0);
        let bounds = self.also.one_in(2);
        let left = alt_os::durability::leave_unsynced(true);
        let synced = match claim {
            Some(claim) => handle.log.sync_for(&handle.claims[claim].ticket),
            None if bounds => handle.log.bound_now(),
            None => handle.log.sync(),
        };
        drop(left);
        self.trace.push(format!(
            "handle {index} syncs with nothing promised (ticket: {}, boundary: {bounds}): \
             {synced:?}",
            claim.is_some()
        ));
        synced.expect("a sync that promises nothing");
        let told = handle.log.take_discarded();
        self.read_anew_since(index, before);
        self.cuts_told(&told, None);
        let handle = &mut self.handles[index];
        match claim {
            Some(claim) if handle.claims[claim].unsynced => drop(handle.claims.remove(claim)),
            Some(_) => {}
            None => {
                let loose = std::mem::take(&mut handle.loose);
                handle.mine.retain(|frame| !loose.contains(frame));
                handle.left.retain(|frame| !loose.contains(frame));
            }
        }
        self.counts.unpromised.fetch_add(1, Ordering::Relaxed);
    }
}
