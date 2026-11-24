//! Steps of a driven sequence that lead a version 2 log, over several
//! boots, into states a few steps drawn one by one seldom reach: ops
//! left unlooked at past where the log ends for a handle that syncs, a
//! checkpoint of a cut that is not on disk when the next writer comes,
//! a cut of ops whose checkpoint the power takes a part of, an op
//! refused with more than one cut past it, and a handle that reads the
//! log anew over a cut made before it opened.

use std::sync::atomic::Ordering;

use alt_oplog::Meta;

use super::v2_driver::Run;
use super::v2_outage::Loss;

fn plain() -> Meta {
    Meta::default()
}

fn linked() -> Meta {
    Meta {
        linked: true,
        ..Meta::default()
    }
}

/// An op that changes the index of workspace `n`, or with `retired`,
/// one with which that workspace is gone.
fn of_workspace(n: u8, retired: bool) -> Meta {
    Meta {
        workspace: Some([n; 8]),
        index_after: (!retired).then_some([n; 20]),
        retired,
        ..Meta::default()
    }
}

impl Run {
    /// One of the steps that are made of several.
    pub fn scene(&mut self, which: u64) {
        match which {
            0 => self.left_past_a_cut_begun(),
            1 => self.appended_past_a_cut_unsynced(),
            2 => self.cut_of_ops_torn(),
            3 => self.cut_twice_past_an_op(),
            4 => self.read_anew_over_a_cut(),
            5 => self.shared_by_callers(),
            6 => self.ticket_given_up(),
            7 => self.told_while_unsure(),
            8 => self.staged_committed_staged(),
            _ => self.cut_begun(),
        }
    }

    /// Appends an op of `meta` through handle `index`, of a writer that
    /// syncs unless `unsynced`; its payload where it was appended.
    fn lay(&mut self, index: usize, meta: Meta, unsynced: bool) -> Option<Vec<u8>> {
        let padding = self.rng.below(120);
        let payload = self.payload(padding);
        self.to_ticket = false;
        self.put(index, (meta, payload.clone()), unsynced, false)
            .then_some(payload)
    }

    /// Another boot leaves an op of a writer that does not sync; a look
    /// that cuts it dies with the header of the log alone telling of
    /// the cut; in the boot after, a handle syncs or sets a boundary
    /// with nothing appended, and then a writer that syncs appends.
    pub fn left_past_a_cut_begun(&mut self) {
        self.room_for(2);
        let index = self.writer();
        if self.also.one_in(2) && self.lay(index, linked(), false).is_none() {
            return;
        }
        let meta = match self.also.one_in(2) {
            true => of_workspace(1 + self.rng.below(3) as u8, false),
            false => plain(),
        };
        let Some(left) = self.lay(index, meta, true) else {
            return;
        };
        self.reboot(true);
        let (cutter, died) = (self.open(false), self.died);
        if !self.lose(&left) {
            return;
        }
        self.settle(cutter, Some(1), false);
        if self.died == died {
            return;
        }
        self.reboot(true);
        let (index, how) = (self.open(false), self.also.below(2));
        self.sync(index, how);
        let writer = self.open(false);
        if self.lay(writer, plain(), false).is_some() {
            self.sync(writer, 0);
        }
        self.counts.scenes[0].fetch_add(1, Ordering::Relaxed);
    }

    /// A cut of what an append left dies with its checkpoint written
    /// and not synced; another writer appends after it; the power goes
    /// and takes a stretch of the checkpoint, where that is not on
    /// disk; the next writer reads the log to its end.
    pub fn appended_past_a_cut_unsynced(&mut self) {
        self.room_for(2);
        let len = match self.also.one_in(2) {
            true => 1 + self.rng.below(120),
            false => 600 + self.rng.below(900),
        };
        self.tear_of(len);
        let (at, died) = (self.log().end, self.died);
        let cutter = self.open(false);
        self.read_on(cutter, true, Some(2));
        if self.died == died {
            return;
        }
        let (writer, unsynced) = (self.open(false), self.also.one_in(2));
        if self.lay(writer, plain(), unsynced).is_none() {
            return;
        }
        self.reboot_as(Loss::Within(at));
        let next = self.open(false);
        self.read_on(next, true, None);
        self.counts.scenes[1].fetch_add(1, Ordering::Relaxed);
    }

    /// A workspace changes its index, a commit follows, and the
    /// workspace is gone; with `synced`, a sync covers that. In the
    /// boot after, the op with which the workspace went is cut. Gives
    /// the payloads of the change and of the op cut, and where that
    /// one started, with the handle that cut and may have died of it
    /// with `dies`.
    fn workspace_gone_and_cut(&mut self, dies: Option<u8>) -> Option<(Vec<u8>, u64)> {
        let index = self.looked_writer(1, false)?;
        let workspace = 1 + self.rng.below(3) as u8;
        let before = self.lay(index, of_workspace(workspace, false), false)?;
        let commit = match self.also.one_in(2) {
            true => linked(),
            false => plain(),
        };
        self.lay(index, commit, false)?;
        let gone = self.lay(index, of_workspace(workspace, true), false)?;
        if self.also.one_in(2) {
            self.sync(index, 0);
        }
        self.reboot(true);
        let at = self.ledger.place_of(&gone);
        let (cutter, ops) = (self.open(false), self.ledger.ops.len());
        if !self.lose(&gone) {
            return None;
        }
        self.settle(cutter, dies, false);
        (self.ledger.ops.len() < ops).then_some((before, at))
    }

    /// The op with which a workspace went is cut, and the cut dies with
    /// its checkpoint written and not synced; the power takes a stretch
    /// of the checkpoint; the next writer makes the cut again and then
    /// looks, with what the change of the index names gone: a sync can
    /// have ended at the op that was cut.
    pub fn cut_of_ops_torn(&mut self) {
        let Some((before, at)) = self.workspace_gone_and_cut(Some(2)) else {
            return;
        };
        self.reboot_as(Loss::Within(at));
        let next = self.open(false);
        self.read_on(next, true, None);
        if self.lose(&before) {
            self.settle(next, None, false);
        }
        self.counts.scenes[2].fetch_add(1, Ordering::Relaxed);
    }

    /// The op with which a workspace went is cut; a ref transaction
    /// and an op are appended after, and the op is cut in the boot
    /// after that; then a look refuses the change of the index, with
    /// what the ref transaction names gone: of the two cuts past it,
    /// a sync can have ended among what the first took.
    pub fn cut_twice_past_an_op(&mut self) {
        let Some((before, _)) = self.workspace_gone_and_cut(None) else {
            return;
        };
        let index = self.handles.len() - 1;
        let Some(dangling) = self.lay(index, linked(), false) else {
            return;
        };
        let Some(last) = self.lay(index, plain(), false) else {
            return;
        };
        self.reboot(true);
        let cutter = self.open(false);
        if !(self.lose(&last) && self.settle(cutter, None, false)) {
            return;
        }
        self.reboot(true);
        let cutter = self.open(false);
        if self.lose(&before) && self.lose(&dangling) {
            self.settle(cutter, None, false);
        }
        self.counts.scenes[3].fetch_add(1, Ordering::Relaxed);
    }

    /// Another boot leaves ops on either side of a cut of what an
    /// append left; a handle holds them all when a look by another,
    /// which cuts from before that cut on, dies with the header of the
    /// log alone telling of it; the handle syncs, and reads the log
    /// anew for that, over the cut made before it opened.
    pub fn read_anew_over_a_cut(&mut self) {
        self.room_for(3);
        let index = self.writer();
        let (first, unsynced) = (self.also.one_in(2), self.also.one_in(2));
        let before = match first {
            true => self.lay(index, linked(), unsynced),
            false => self.lay(index, plain(), unsynced),
        };
        let Some(before) = before else {
            return;
        };
        let len = 20 + self.rng.below(600);
        self.tear_of(len);
        let index = self.open(false);
        self.read_on(index, true, None);
        let unsynced = self.also.one_in(2);
        if self.lay(index, plain(), unsynced).is_none() {
            return;
        }
        self.reboot(true);
        let held = self.open(false);
        if self.also.one_in(3) {
            self.settle(held, None, false);
        }
        let _ = self.beside != super::v2_beside::Beside::Kept
            && std::fs::remove_file(self.path().join("tail")).is_ok();
        let (cutter, died) = (self.open(false), self.died);
        if self.also.one_in(2) {
            self.lose(&before);
        }
        self.settle(cutter, Some(1), true);
        if self.died == died {
            return;
        }
        let how = self.also.below(2);
        self.sync(held, how);
        if self.lay(held, plain(), false).is_some() {
            self.sync(held, 0);
        }
        self.counts.scenes[4].fetch_add(1, Ordering::Relaxed);
    }
}
