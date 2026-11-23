//! What the calls a driver makes on handles of a version 2 log come to,
//! taken into what it knows of the log and held to that.

use std::sync::atomic::Ordering;

use alt_oplog::{Discarded, OpLogError, Unconfirmed};

use super::v2_driver::{Run, name};
use super::v2_ledger::Entry;
use super::v2_meanwhile::Outcome;
use super::v2_model::Laid;

impl Run {
    /// Takes in what a handle tells was cut off the log; `cut` are the
    /// ops the look of the call had cut.
    pub fn cuts_told(&mut self, told: &[Discarded], cut: Option<&[Entry]>) {
        if !told.is_empty() {
            // a cut syncs the checkpoint it leaves, and what was noted
            // of where the log ends holds no longer
            self.cut_synced = true;
            self.ledger.hides.clear();
            self.ledger.begun = None;
        }
        self.ledger.reconciled(told, cut);
    }

    /// What a call on handle `index` came to: nothing where it was told
    /// its ops were cut or failed as `may_fail` allows. A handle is told
    /// so only when they were, and holds the log again once it has read
    /// it anew.
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
                self.trace.push("  it is told its ops were cut".into());
                let len = self.file_len();
                let handle = &mut self.handles[index];
                let end = handle.log.appended_len();
                let begun = self.ledger.begun.is_some_and(|at| at < end);
                assert!(
                    begun || !self.ledger.holds(handle.log.anchor()),
                    "handle {index} is told its ops were cut: the log holds them"
                );
                handle.log.reopen().expect("the log reads anew");
                handle.seen.push(len);
                assert!(self.ledger.holds(handle.log.anchor()));
                handle.log.follow().expect("it holds the log again");
                self.counts.cut_back.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(OpLogError::Io(_)) if may_fail => {
                self.trace.push("  it fails as it was made to".into());
                None
            }
            Err(other) => panic!("{other:?}"),
        }
    }

    /// Takes in what a look came to: the ops `asked` about, with the
    /// answers, up to the op at `stopped` where it found one not vouched
    /// for. Every op of `unsettled` before that has to be among them.
    pub fn looked(&mut self, unsettled: &[Laid], asked: &[(Vec<u8>, bool)], stopped: Option<u64>) {
        let names: Vec<String> = asked.iter().map(|(payload, _)| name(payload)).collect();
        self.trace
            .push(format!("  asked of {names:?}, stopped at {stopped:?}"));
        let vouched = asked.iter().filter(|(_, held)| *held);
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

    /// Takes in what a writer's look came to: the ops it cut are the one
    /// `refused` and all after it, and what the handle tells as cut is
    /// those.
    pub fn settled(
        &mut self,
        unsettled: &[Laid],
        refused: &Option<Vec<u8>>,
        asked: &[(Vec<u8>, bool)],
        (cut, told): (Option<Unconfirmed>, Vec<Discarded>),
    ) {
        let gone = cut.map(|cut| {
            let refused = refused.as_ref().expect("an op was refused");
            let gone = self.ledger.cut_from(refused, self.model.synced);
            assert_eq!((cut.at, cut.ops), (gone[0].at, gone.len()));
            gone
        });
        self.cuts_told(&told, gone.as_deref());
        self.looked(unsettled, asked, cut.map(|cut| cut.at));
        self.ledger.begun = None;
    }

    /// Takes in what another handle did while a writer had given up the
    /// lock; `unsettled` are the ops that were left to look at then.
    pub fn meanwhile_done(&mut self, unsettled: &[Laid]) {
        self.meanwhile.plan(None);
        for outcome in self.meanwhile.done() {
            self.counts.meanwhile.fetch_add(1, Ordering::Relaxed);
            match outcome {
                Outcome::Appended {
                    meta,
                    payload,
                    head,
                    told,
                } => {
                    self.trace
                        .push(format!("  meanwhile {} is appended", name(&payload)));
                    self.cuts_told(&told, None);
                    self.appended(payload, &meta, head);
                }
                Outcome::Looked {
                    refused,
                    asked,
                    cut,
                    told,
                } => {
                    let refuses = refused.as_deref().map(name);
                    self.trace.push(format!(
                        "  meanwhile a writer settles, refusing {refuses:?}"
                    ));
                    self.settled(unsettled, &refused, &asked, (cut, told));
                }
                Outcome::Failed(why) => {
                    assert!(why.starts_with("Io("), "{why}");
                    self.trace
                        .push(format!("  meanwhile a handle fails: {why}"));
                }
            }
        }
    }
}
