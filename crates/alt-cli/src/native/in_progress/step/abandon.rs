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

/// What was changed since the step `begun` began was cut short is in the
/// way of giving it up, and how it is given up all the same.
fn changed(what: &str, begun: OpId) -> Box<dyn std::error::Error> {
    refusal(format!(
        "{what} was changed after the operation was interrupted; `alt operation abandon {begun} \
         --keep-changes` gives the operation up and leaves it as it is"
    ))
}

impl NativeRepo<'_> {
    /// Each file the step wrote, from what is there now to what the step
    /// found. A file that is none of the states the step recorded was
    /// changed since: the step is not given up over it unless asked to
    /// `keep` it, and then it is left as it is. The files kept so, by name.
    fn files_back(
        &self,
        told: &Told,
        begun: OpId,
        keep: bool,
    ) -> Res<(Vec<FileChange>, Vec<String>)> {
        let mut known: BTreeMap<&[u8], Recorded> = BTreeMap::new();
        for change in told.checkouts.iter().flat_map(|c| &c.files) {
            let path = known
                .entry(&change.path)
                .or_insert((&change.before, Vec::new()));
            path.1.extend([&change.before, &change.after]);
        }
        let paths: HashSet<&[u8]> = known.keys().copied().collect();
        let (mut files, mut kept) = (Vec::new(), Vec::new());
        for (path, (found, states)) in &known {
            let named = BString::from(path.to_vec());
            let gitlink = states
                .iter()
                .any(|s| s.as_ref().is_some_and(|s| s.mode == 0o160000));
            let now = self.observed(&named, false, gitlink, &|p| paths.contains(p))?;
            // nothing is lost where a file is gone
            if now.is_some() && !states.contains(&&now) {
                if !keep {
                    return Err(changed(&format!("'{named}'"), begun));
                }
                kept.push(named.to_string());
                continue;
            }
            if now != **found {
                files.push(FileChange {
                    path: path.to_vec(),
                    before: now,
                    after: (*found).clone(),
                });
            }
        }
        Ok((files, kept))
    }

    /// The checkout that puts each file the step wrote, and the index, as
    /// the step found them, from what is there now, none where they are
    /// so already; and what is left as it
    /// is because asked to `keep` it: a file that was changed since, and an
    /// index that is neither as the step found it nor as it left it.
    fn checkout_back(
        &self,
        told: &Told,
        begun: OpId,
        keep: bool,
    ) -> Res<(Option<CheckoutIntent>, Vec<String>)> {
        let Some(first) = told.checkouts.first() else {
            return Ok((None, Vec::new()));
        };
        let (files, mut kept) = self.files_back(told, begun, keep)?;
        let index = canonical_index(self.index()?, self.store.algo()?);
        let now = hash(&index);
        let recorded = |c: &CheckoutIntent| c.index_before == now || c.index_after == now;
        let index_after = match told.checkouts.iter().any(recorded) {
            true => first.index_before.clone(),
            false if keep => {
                kept.push("the index".to_owned());
                self.snapshot(&index)?
            }
            false => return Err(changed("the index", begun)),
        };
        let back = CheckoutIntent {
            version: 1,
            workspace: first.workspace.clone(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: now,
            index_after,
            files,
        };
        // where all is as the step found it there is no checkout to make:
        // the mark that gives the step up says all there is to say
        if back.files.is_empty() && back.index_before == back.index_after {
            return Ok((None, kept));
        }
        self.ensure_directories_replaceable(&back)?;
        Ok((Some(back), kept))
    }

    /// The ref changes that take back what the step moved; of the notes
    /// ref, only the notes of the commits it made.
    fn refs_back(&mut self, told: &Told) -> Res<Vec<RefChange>> {
        let mut back = Vec::new();
        for moved in net_refs(&told.refs) {
            let change = RefChange {
                old: read(&moved.new)?,
                new: read(&moved.old)?,
                name: moved.name,
            };
            if self.store.refs.get(&change.name) != change.old.as_ref() {
                let why = format!("{} was moved since the operation moved it", change.name);
                return Err(refusal(why));
            }
            back.push(change);
        }
        self.ensure_no_force(&back)?;
        back.extend(self.notes_back(&told.refs)?);
        self.ensure_no_force(&back)?;
        Ok(back)
    }

    /// Writes that the step `begun` began is given up, and with that the
    /// `refs` moved back and the `checkout` that puts its files back. The
    /// state files are as the step found them with the record that takes
    /// the rest back, or at once where there is nothing else.
    fn give_up(
        &mut self,
        begun: &alt_oplog::Op,
        mark: &StepMark,
        refs: Vec<RefChange>,
        checkout: Option<CheckoutIntent>,
    ) -> Res<()> {
        let said = StepMark {
            version: 1,
            workspace: mark.workspace.clone(),
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            begin: Some(begun.id.to_string()),
            inverse_of: None,
            before: Vec::new(),
            state: mark.before.clone(),
            with_effect: checkout.is_some() || !refs.is_empty(),
            gives_up: true,
        };
        let actor = self.id.actor("recover");
        let payload = encode(STEP_MARK, &said)?;
        self.store.refs.record_op(&actor, now_ms(), &payload)?;
        if refs.is_empty() {
            if let Some(checkout) = &checkout {
                self.record_checkout("recover", checkout)?;
            }
            return Ok(());
        }
        self.checkout_intent = checkout;
        let effects = recovery::RefEffects::RefsOnly;
        let moved = self.commit_refs_gated("recover", &refs, None, effects);
        self.checkout_intent = None;
        moved?;
        Ok(())
    }

    /// Gives up the step of this workspace that was cut short: the
    /// workspace is as before the command of that step. Refused, with
    /// nothing changed, where it is no longer as the step left it. The
    /// state files the step had made, none where there was no such step,
    /// and what was left as it is because asked to `keep` it.
    pub(in crate::native) fn abandon_open_step(
        &mut self,
        keep: bool,
    ) -> Res<(Vec<String>, Vec<String>)> {
        let Some((begun, mark, false)) = self.last_step_mark()? else {
            return Ok(Default::default());
        };
        let told = self.told_after(&begun, &mark)?;
        if told.foreign {
            let id = begun.id;
            let why = format!(
                "operation {id} was interrupted and another program has written since: it \
                 cannot be given up; see `alt status`, and close it with `alt operation \
                 recover {id}`"
            );
            return Err(refusal(why));
        }
        let (checkout, kept) = self.checkout_back(&told, begun.id, keep)?;
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
        self.give_up(&begun, &mark, refs, checkout)?;
        self.recover_open_step(false)?;
        Ok((made, kept))
    }

    /// `alt operation abandon <id> [--keep-changes]`: gives up the step
    /// that was cut short and `id` is a record of.
    pub fn operation_abandon(
        &mut self,
        id: &str,
        keep: bool,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
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
        let (_, kept) = self.abandon_open_step(keep)?;
        if json {
            use crate::json::Json;
            let fields = vec![
                ("operation_id", Json::str(begun.id.to_string())),
                ("abandoned", Json::Bool(true)),
                ("kept", Json::Array(kept.iter().map(Json::str).collect())),
            ];
            crate::json::emit(out, fields)?;
        } else {
            let id = begun.id;
            let but = match kept.is_empty() {
                true => String::new(),
                false => format!(
                    ", but for what was changed since and is left as it is: {}",
                    kept.join(", ")
                ),
            };
            writeln!(
                out,
                "{id} given up: the workspace is as it was before the command{but}"
            )?;
        }
        Ok(())
    }
}
