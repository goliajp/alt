//! A step that was cut short and cannot be brought to an end, given up.
//!
//! The step is taken back as a whole, by one more record of the step
//! itself: the refs it moved are moved back and each file it wrote is put
//! as the step found it, together with the state files. A checkout the
//! step left unfinished is closed by that record and not gone on with.
//! From there the step is brought to an end as any other is, so that one
//! killed while it is given up ends the same way; the record that closes
//! it tells of no change, and an undo passes it over.

use std::collections::{BTreeMap, HashSet};

use super::super::super::checkout_recovery::{CheckoutIntent, FileChange, FileState};
use super::super::super::recovery::{canonical_index, encode, hash};
use super::super::super::*;
use super::mark::{StateText, StepMark};
use super::recover::Told;
use super::{STEP_MARK, net_refs, read};

/// A path as the step found it, and every state the step recorded there.
type Recorded<'a> = (&'a Option<FileState>, Vec<&'a Option<FileState>>);

fn refusal(why: impl Into<String>) -> Box<dyn std::error::Error> {
    crate::machine::Failure::refusal("precondition_failed", why).into()
}

impl NativeRepo<'_> {
    /// The checkout that puts each file the step wrote, and the index, as
    /// the step found them: from what is there now, which for each must
    /// be one of the states the step recorded.
    fn checkout_back(&self, told: &Told) -> Res<Option<CheckoutIntent>> {
        let Some(first) = told.checkouts.first() else {
            return Ok(None);
        };
        let mut known: BTreeMap<&[u8], Recorded> = BTreeMap::new();
        for change in told.checkouts.iter().flat_map(|c| &c.files) {
            let path = known
                .entry(&change.path)
                .or_insert((&change.before, Vec::new()));
            path.1.extend([&change.before, &change.after]);
        }
        let paths: HashSet<&[u8]> = known.keys().copied().collect();
        let mut files = Vec::new();
        for (path, (found, states)) in &known {
            let named = BString::from(path.to_vec());
            let gitlink = states
                .iter()
                .any(|s| s.as_ref().is_some_and(|s| s.mode == 0o160000));
            let now = self.observed(&named, false, gitlink, &|p| paths.contains(p))?;
            if !states.contains(&&now) {
                let why =
                    format!("'{named}' is neither as the operation found it nor as it left it");
                return Err(refusal(why));
            }
            files.push(FileChange {
                path: path.to_vec(),
                before: now,
                after: (*found).clone(),
            });
        }
        let index = hash(&canonical_index(self.index()?, self.store.algo()?));
        let recorded = |c: &CheckoutIntent| c.index_before == index || c.index_after == index;
        if !told.checkouts.iter().any(recorded) {
            return Err(refusal(
                "the index is neither as the operation found it nor as it left it",
            ));
        }
        let back = CheckoutIntent {
            version: 1,
            workspace: first.workspace.clone(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: index,
            index_after: first.index_before.clone(),
            files,
        };
        self.ensure_directories_replaceable(&back)?;
        Ok(Some(back))
    }

    /// The ref changes that take back what the step moved. The note of a
    /// commit it made stays where another workspace wrote one on top.
    fn refs_back(&self, told: &Told) -> Res<Vec<RefChange>> {
        let mut back = Vec::new();
        for moved in net_refs(&told.refs) {
            let change = RefChange {
                old: read(&moved.new)?,
                new: read(&moved.old)?,
                name: moved.name,
            };
            if self.store.refs.get(&change.name) == change.old.as_ref() {
                back.push(change);
            } else if change.name != crate::meta::NOTES_REF {
                let why = format!("{} was moved since the operation moved it", change.name);
                return Err(refusal(why));
            }
        }
        self.ensure_no_force(&back)?;
        Ok(back)
    }

    /// Gives up the step of this workspace that was cut short: the
    /// workspace is as before the command of that step. Refused, with
    /// nothing changed, where it is no longer as the step left it. The
    /// state files the step had made, none where there was no such step.
    pub(in crate::native) fn abandon_open_step(&mut self) -> Res<Vec<String>> {
        let Some((begun, mark, false)) = self.last_step_mark()? else {
            return Ok(Vec::new());
        };
        let Some(told) = self.told_after(&begun, &mark)? else {
            let why = "another program has written since the operation was interrupted";
            return Err(refusal(why));
        };
        let checkout = self.checkout_back(&told)?;
        let refs = self.refs_back(&told)?;
        let made = |file: &&StateText| {
            file.text.is_none() && told.state.get(&file.name).is_some_and(Option::is_some)
        };
        let made: Vec<String> = mark
            .before
            .iter()
            .filter(made)
            .map(|f| f.name.clone())
            .collect();
        // the state files are as the step found them with the record that
        // takes the rest back, or at once where there is nothing else
        let said = StepMark {
            version: 1,
            workspace: mark.workspace.clone(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            begin: Some(begun.id.to_string()),
            inverse_of: None,
            before: Vec::new(),
            state: mark.before.clone(),
            with_effect: checkout.is_some() || !refs.is_empty(),
        };
        let actor = self.id.actor("recover");
        let payload = encode(STEP_MARK, &said)?;
        self.store.refs.record_op(&actor, now_ms(), &payload)?;
        if refs.is_empty() {
            if let Some(checkout) = &checkout {
                self.record_checkout("recover", checkout)?;
            }
        } else {
            self.checkout_intent = checkout;
            let effects = recovery::RefEffects::RefsOnly;
            let moved = self.commit_refs_gated("recover", &refs, None, effects);
            self.checkout_intent = None;
            moved?;
        }
        self.recover_open_step()?;
        Ok(made)
    }

    /// `alt operation abandon <id>`: gives up the step that was
    /// cut short and `id` is a record of.
    pub fn operation_abandon(&mut self, id: &str, json: bool, out: &mut impl Write) -> Res<()> {
        self.ensure_may_recover()?;
        let op = self.operation_by_id(id)?;
        let open = self.last_step_mark()?.filter(|(_, _, closed)| !closed);
        let nothing = "the operation is no part of an interrupted merge, cherry-pick, revert or \
                       rebase command of this workspace: there is nothing to give up";
        let (begun, mark, _) = open.ok_or_else(|| refusal(nothing))?;
        let ops = self.store.refs.oplog().ops()?;
        let at = |id: OpId| ops.iter().position(|known| known.id == id);
        let own = self.operation_info(&op)?.0.as_ref() == Some(&mark.workspace);
        if !own || at(op.id) < at(begun.id) {
            return Err(refusal(nothing));
        }
        self.abandon_open_step()?;
        if json {
            use crate::json::Json;
            let fields = vec![
                ("operation_id", Json::str(begun.id.to_string())),
                ("abandoned", Json::Bool(true)),
            ];
            crate::json::emit(out, fields)?;
        } else {
            let id = begun.id;
            writeln!(
                out,
                "{id} given up: the workspace is as it was before the command"
            )?;
        }
        Ok(())
    }

    /// Before an `--abort`: a step that was cut short is brought to an
    /// end, or given up where it cannot be. Whether it was given up and
    /// had made the state file `state`, so that the operation the abort
    /// is of is given up with it.
    pub fn begin_abort(&mut self, state: &str) -> Res<bool> {
        self.ensure_may_recover()?;
        if self.recover_open_step().is_ok() {
            return Ok(false);
        }
        let made = self.abandon_open_step()?;
        Ok(made.iter().any(|name| name == state))
    }
}
