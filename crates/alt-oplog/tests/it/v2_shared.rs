//! Steps of a driven sequence in which callers share one handle of a
//! version 2 log and frames it wrote for them are cut by another
//! writer: a caller that syncs beside one that is told to leave what it
//! writes unsynced, a caller that gives up its ticket before another
//! has the handle sync within its write, and a caller that is told its
//! ops were cut while the handle cannot tell which.

use std::sync::atomic::Ordering;

use alt_oplog::{Meta, OpLog, OpLogError};

use super::v2_beside::Beside;
use super::v2_driver::{Run, name};
use super::v2_meanwhile::{refused_of, says};
use super::v2_owed::Wrote;

impl Run {
    /// Closes handles until `more` can be opened with none closed for
    /// it.
    pub fn room_for(&mut self, more: usize) {
        while self.handles.len() + more > 4 {
            self.trace.push("handle 0 is closed".into());
            self.handles.remove(0);
        }
    }

    /// A writer opened now, with room for `more` handles after it, that
    /// has looked at what was left; it leaves its syncs to a sink with
    /// `deferred`.
    pub fn looked_writer(&mut self, more: usize, deferred: bool) -> Option<usize> {
        self.room_for(more + 1);
        self.to_defer = deferred;
        let index = self.open(false);
        self.to_defer = false;
        self.settle(index, None, false).then_some(index)
    }

    /// What the op of `payload` names is gone, where it may be: another
    /// boot left the op past where the log is synced, no sync saw to
    /// it, and nobody was told since that what it names is stored.
    pub fn lose(&mut self, payload: &[u8]) -> bool {
        let synced = self.synced_now();
        let model = &self.model;
        let held = self.ledger.ops.iter().find(|op| op.payload == payload);
        let at_risk = held.is_some_and(|op| op.at >= synced)
            && model.written.get(payload) != Some(&model.boot)
            && !model.durable.contains(payload)
            && !model.vouched.contains(payload);
        let lost = at_risk && model.stored.lock().unwrap().remove(payload);
        if lost {
            self.trace
                .push(format!("what {} names is gone", name(payload)));
        }
        lost
    }

    /// Whoever is asked while `look` runs does not find what the op of
    /// `payload` names, an op of the running boot that no sync saw to:
    /// what it names is there again after, where the op is in the log
    /// still. Whether the op was cut.
    pub fn refused_while(&mut self, payload: &[u8], look: impl FnOnce(&mut Self)) -> bool {
        let synced = self.synced_now();
        let held = self.ledger.ops.iter().find(|op| op.payload == payload);
        let may = held.is_some_and(|op| op.at >= synced) && !self.model.durable.contains(payload);
        if !(may && self.model.stored.lock().unwrap().remove(payload)) {
            return false;
        }
        self.trace
            .push(format!("what {} names is not found", name(payload)));
        look(self);
        let cut = self.ledger.ops.iter().all(|op| op.payload != payload);
        if !cut {
            self.model.stored.lock().unwrap().insert(payload.to_vec());
        }
        cut
    }

    /// Another writer, which finds no note beside the log, looks at
    /// what was left and cuts what is not vouched for; whether it cut.
    pub fn another_looks(&mut self) -> bool {
        let unsettled = self.unsettled_before(u64::MAX);
        let _ = std::fs::remove_file(self.path().join("tail"));
        let mut cutter = OpLog::open(self.path()).expect("the log opens");
        let (stored, mut asked) = (self.model.stored.clone(), Vec::new());
        let cut = cutter.settle(says(&stored, &mut asked));
        let told = cutter.take_discarded();
        drop(cutter);
        self.trace.push("a writer looks".into());
        match cut {
            Ok(cut) => self.settled(&unsettled, &asked, (cut, told)),
            Err(why @ (OpLogError::Kept { .. } | OpLogError::Lost { .. })) => {
                self.cut_refused(&refused_of(&asked), &asked, &why, &told);
                false
            }
            Err(other) => panic!("{other:?}"),
        }
    }

    /// Appends `op` through handle `index` on a ticket and has the
    /// handle sync, or set a boundary, within that write, as a commit
    /// does. That sync answers for no frame: it tells of no cut.
    pub fn put_within(&mut self, index: usize, op: (Meta, Vec<u8>), bounds: bool) -> bool {
        let (meta, payload) = op;
        let unsettled = self.unsettled_before(u64::MAX);
        self.trace.push(format!(
            "handle {index} appends {} on a ticket and syncs within (boundary: {bounds})",
            name(&payload)
        ));
        self.calls();
        let before = (self.handles[index].log.reads(), self.file_len());
        let written = self.handles[index].log.ticketed(|log| {
            log.append_with("driver", 1, &payload, &meta)?;
            let head = log.anchor().unwrap();
            let synced = match bounds {
                true => log.bound_now(),
                false => log.sync(),
            };
            Ok::<_, OpLogError>((head, synced))
        });
        if !matches!(written, Err(OpLogError::CutBack)) {
            self.ledger.begun = None;
        }
        self.calls();
        self.meanwhile_done(&unsettled);
        let Some(((head, synced), ticket)) = self.took(index, written, false) else {
            return false;
        };
        self.read_anew_since(index, before);
        self.appended("driver", payload, (&meta, false), head);
        self.trace
            .push(format!("  the sync within says {synced:?}"));
        let handle = &mut self.handles[index];
        if handle.sink.is_some() && synced.is_ok() {
            handle.head_left = Some((handle.log.anchor(), handle.log.appended_len()));
        }
        match ticket.is_empty() {
            // the sync within covered the frame: its caller is owed
            // nothing more
            true => self.told_synced(&[Wrote::Op(head)], "an empty ticket"),
            false => self.wrote(index, Wrote::Op(head), Some(ticket), false),
        }
        match synced {
            Ok(()) => {}
            Err(OpLogError::Io(_)) if self.lacked_now() => {}
            Err(OpLogError::Unjudged { at }) => self.unjudged(index, at),
            Err(other) => panic!("a sync asked within a write on a ticket says {other:?}"),
        }
        self.counts.shared[3].fetch_add(1, Ordering::Relaxed);
        true
    }

    /// A caller that syncs and one that is told to leave what it writes
    /// unsynced share a handle. The frames of the first are cut, before
    /// or after it had them left to a sink, and the handle finds that
    /// or does not; the second asks, with nothing promised; then the
    /// first asks, and is told of the cut.
    pub fn shared_by_callers(&mut self) {
        if self.beside == Beside::Kept {
            return;
        }
        let deferred = self.also.one_in(2);
        let Some(index) = self.looked_writer(0, deferred) else {
            return;
        };
        let op = self.op();
        if !self.put(index, op.clone(), false, false) {
            return;
        }
        if self.also.one_in(2) {
            let loose = self.op();
            self.put(index, loose, true, false);
        }
        if deferred && self.also.one_in(2) {
            // left to the sink, which is not asked yet
            self.leaves_only = true;
            self.sync(index, 0);
            self.leaves_only = false;
        }
        if !self.refused_while(&op.1, |run| _ = run.another_looks()) {
            return;
        }
        if self.also.one_in(2) {
            self.read_on(index, false, None);
        }
        self.sync_unpromised_of(index);
        for how in [0, 2] {
            self.sync(index, how);
        }
        self.counts.shared[0].fetch_add(1, Ordering::Relaxed);
    }

    /// A caller has a handle write on a ticket and gives the ticket
    /// up; its op is cut. Another has the handle append and sync
    /// within its write, and is answered by its ticket. Whoever asks
    /// the handle without one after is answered for what was given up.
    pub fn ticket_given_up(&mut self) {
        if self.beside == Beside::Kept {
            return;
        }
        let deferred = self.also.one_in(2);
        let Some(index) = self.looked_writer(0, deferred) else {
            return;
        };
        let op = self.op();
        self.to_ticket = true;
        if !self.put(index, op.clone(), false, false) {
            return;
        }
        let handle = &mut self.handles[index];
        let mut given_up = handle.claims.pop().expect("it wrote on a ticket");
        handle.mine.append(&mut given_up.frames);
        drop(given_up);
        self.trace.push("its ticket is given up".into());
        if !self.refused_while(&op.1, |run| _ = run.another_looks()) {
            return;
        }
        self.read_on(index, false, None);
        let (next, bounds) = (self.op(), self.also.one_in(2));
        if self.put_within(index, next, bounds) {
            self.sync(index, 2);
        }
        self.sync(index, 0);
        self.counts.shared[1].fetch_add(1, Ordering::Relaxed);
    }

    /// A handle is told that its ops were cut where it cannot tell
    /// whether its own op is: the op was cut and the log ends, for the
    /// handle, at a cut of what an append left that died; or a cut of
    /// the op itself was begun and died. Its caller is told or is not,
    /// and the sync the handle makes after is held to that.
    pub fn told_while_unsure(&mut self) {
        if self.beside == Beside::Kept {
            return;
        }
        let Some(index) = self.looked_writer(1, false) else {
            return;
        };
        // an op that ends well past where the log ends once it is cut
        let padding = 600 + self.rng.below(300);
        let payload = self.payload(padding);
        if !self.put(index, (Meta::default(), payload.clone()), false, false) {
            return;
        }
        let died = self.died;
        match self.also.one_in(2) {
            true => {
                if !self.refused_while(&payload, |run| _ = run.another_looks()) {
                    return;
                }
                let len = 20 + self.rng.below(200);
                self.tear_of(len);
                let cutter = self.open(false);
                self.read_on(cutter, true, Some(1));
            }
            false => {
                let _ = std::fs::remove_file(self.path().join("tail"));
                let cutter = self.open(false);
                self.refused_while(&payload, |run| _ = run.settle(cutter, Some(1), false));
            }
        }
        if self.died == died {
            return;
        }
        self.read_on(index, false, None);
        match self.also.below(3) {
            0 => self.read_on(index, true, None),
            1 => _ = self.another_looks(),
            _ => {}
        }
        for how in [0, 0, 2] {
            self.sync(index, how);
        }
        self.counts.shared[2].fetch_add(1, Ordering::Relaxed);
    }
}
