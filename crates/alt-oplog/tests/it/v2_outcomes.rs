//! What the calls a driver makes on handles of a version 2 log come to,
//! taken into what it knows of the log and held to that.

use std::sync::atomic::Ordering;

use alt_oplog::{Discarded, OpLogError, Unconfirmed};

use super::v2_driver::{Run, name};
use super::v2_ledger::Entry;
use super::v2_meanwhile::{Outcome, refused_of};
use super::v2_model::Laid;

impl Run {
    /// Takes in what a handle tells was cut off the log; `cut` are the
    /// ops the look of the call had cut.
    pub fn cuts_told(&mut self, told: &[Discarded], cut: Option<&[Entry]>) {
        if !told.is_empty() {
            // what was noted of where the log ends holds no longer
            self.ledger.hides.clear();
            self.ledger.begun = None;
        }
        self.ledger.reconciled(told, cut);
    }

    /// What a call on handle `index` came to that reads the log or
    /// appends to it: nothing where it was told that ops it holds were
    /// cut, or failed as `may_fail` allows. A handle is told so only
    /// when ops it holds are gone or a cut of them was begun. It reads
    /// the log anew then, with its caller told or with nobody told, and
    /// holds the log again.
    pub fn took<T>(
        &mut self,
        index: usize,
        did: Result<T, OpLogError>,
        may_fail: bool,
    ) -> Option<T> {
        let told = self.handles[index].log.take_discarded();
        self.cuts_told(&told, None);
        match did {
            Ok(value) => Some(value),
            Err(OpLogError::CutBack) => {
                let tells = self.also.one_in(2);
                self.trace.push(format!(
                    "  it is told its ops were cut, and tells its caller: {tells}"
                ));
                let len = self.file_len();
                self.handle_told(index);
                let handle = &mut self.handles[index];
                match tells {
                    true => handle.log.reopen().expect("the log reads anew"),
                    false => handle.log.reread().expect("the log reads anew"),
                }
                handle.seen.push(len);
                if tells {
                    self.forget_gone(index);
                } else {
                    self.counts.untold.fetch_add(1, Ordering::Relaxed);
                }
                let handle = &mut self.handles[index];
                assert!(self.ledger.holds(handle.log.anchor()));
                handle.log.follow().expect("it holds the log again");
                self.counts.cut_back.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(OpLogError::Io(_)) if may_fail || self.lacked_now() => {
                self.trace.push("  it fails as it was made to".into());
                None
            }
            // a writer that syncs looks before it appends
            Err(OpLogError::Unjudged { at }) => {
                self.unjudged(index, at);
                None
            }
            Err(other) => panic!("{other:?}"),
        }
    }

    /// Handle `index` is told that ops it holds were cut, by a call
    /// that reads the log or appends to it: ops it holds are gone, or
    /// a cut of them was begun.
    fn handle_told(&mut self, index: usize) {
        let handle = &self.handles[index];
        let (last, end) = (handle.log.anchor(), handle.log.appended_len());
        let begun = self.ledger.begun.is_some_and(|at| at < end);
        let held = self.ledger.holds(last);
        assert!(
            begun || !held,
            "handle {index} is told its ops were cut: the log holds them, up to {end}"
        );
        if held {
            self.counts.cut_back_begun.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Takes in what a look came to: the ops `asked` about, with the
    /// answers, up to the op at `stopped` where it found one not vouched
    /// for. Every op of `unsettled` before that has to be among them.
    pub fn looked(&mut self, unsettled: &[Laid], asked: &[(Vec<u8>, bool)], stopped: Option<u64>) {
        let names: Vec<String> = asked.iter().map(|(payload, _)| name(payload)).collect();
        self.trace
            .push(format!("  asked of {names:?}, stopped at {stopped:?}"));
        // what it asks about past an op it was told is not held, it
        // asks to tell whether a frame there is one to keep
        let vouched = asked.iter().take_while(|(_, held)| *held);
        self.model
            .vouched
            .extend(vouched.map(|(payload, _)| payload.clone()));
        for op in unsettled
            .iter()
            .filter(|op| stopped.is_none_or(|at| op.at < at))
        {
            assert!(
                asked.iter().any(|(payload, _)| *payload == op.payload),
                "the look passed over {} at {}, which another boot left and nobody vouched for",
                name(&op.payload),
                op.at
            );
        }
    }

    /// Takes in what a writer's look came to. It cut the log when it was
    /// told of an op that it is not held, and then only: the ops cut are
    /// that one and all after it, what the look gives back says so, and
    /// what the handle tells as cut is those. Whether it cut.
    pub fn settled(
        &mut self,
        unsettled: &[Laid],
        asked: &[(Vec<u8>, bool)],
        (cut, told): (Option<Unconfirmed>, Vec<Discarded>),
    ) -> bool {
        let refused = refused_of(asked);
        let gone = refused.as_ref().map(|refused| self.cut_took(refused));
        let cut_at = gone.as_ref().map(|gone| (gone[0].at, gone.len()));
        assert_eq!(
            cut.map(|cut| (cut.at, cut.ops)),
            cut_at,
            "a look says it cut the log otherwise than at the op it was told is not held"
        );
        self.cuts_told(&told, gone.as_deref());
        self.looked(unsettled, asked, cut_at.map(|(at, _)| at));
        self.ledger.begun = None;
        gone.is_some()
    }

    /// Takes in what another handle did while a writer had given up the
    /// lock; `unsettled` are the ops that were left to look at then.
    pub fn meanwhile_done(&mut self, unsettled: &[Laid]) {
        self.meanwhile.plan(None);
        for outcome in self.meanwhile.done() {
            let done = !matches!(outcome, Outcome::Failed(_));
            self.counts
                .meanwhile
                .fetch_add(done as u64, Ordering::Relaxed);
            match outcome {
                Outcome::Appended {
                    meta,
                    payload,
                    unsynced,
                    head,
                    told,
                } => {
                    self.trace
                        .push(format!("  meanwhile {} is appended", name(&payload)));
                    self.cuts_told(&told, None);
                    self.appended("meanwhile", payload, (&meta, unsynced), head);
                }
                Outcome::Refused { asked, why, told } => {
                    self.cut_refused(&refused_of(&asked), &asked, &why, &told)
                }
                Outcome::Looked { asked, cut, told } => {
                    self.trace.push("  meanwhile a writer settles".into());
                    let cut = self.settled(unsettled, &asked, (cut, told));
                    let cuts = &self.counts.meanwhile_cuts;
                    cuts.fetch_add(cut as u64, Ordering::Relaxed);
                }
                Outcome::Failed(why) => {
                    let looks_first = why.starts_with("Unjudged");
                    assert!(why.starts_with("Io(") || looks_first, "{why}");
                    self.trace
                        .push(format!("  meanwhile a handle fails: {why}"));
                }
            }
        }
    }
}
