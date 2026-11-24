//! What a caller of a driven sequence that asks without a ticket knows
//! of its frames beside the answers of syncs: a handle read the log anew
//! with that caller told that ops were cut, or cut ops itself.

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
}
