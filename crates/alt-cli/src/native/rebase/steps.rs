//! One step of a rebase: a name set, a stand taken elsewhere, a merge
//! commit made again, a command asked for, a commit replayed.

use super::super::sequence_editor::Action;
use super::super::*;
use super::Picked;
use super::state::{RebaseState, Step};

impl NativeRepo<'_> {
    /// Runs `step`, the first of those `state` had left. `Some` where the
    /// rebase stops at it, with whether it stopped on a conflict.
    pub(super) fn run_step(
        &mut self,
        state: &mut RebaseState,
        step: Step,
        json: bool,
        out: &mut impl Write,
    ) -> Res<Option<bool>> {
        match step {
            Step::Run(command) => {
                // whoever gave this command runs that one, once what
                // was done so far holds and nothing is held
                state.ran = true;
                self.save_rebase_state(state)?;
                self.settle_state()?;
                crate::exec_step::ask(command, self.root.clone());
                Ok(Some(false))
            }
            Step::Label(name) => {
                let at = self.rebase_head()?;
                state.labels.retain(|label| label.0 != name);
                state.labels.push((name, at));
                Ok(None)
            }
            Step::Reset(to) => {
                let (head, to) = (self.rebase_head()?, self.named_in(state, &to)?);
                if to != head {
                    self.save_rebase_state(state)?;
                    self.step_onto(head, to)?;
                }
                Ok(None)
            }
            Step::Merge(commit, brought) => self.merge_again(state, commit, &brought, json, out),
            Step::Replay(Action::Drop, _) => Ok(None),
            Step::Replay(action, commit) => self.replay(state, action, commit, json, out),
        }
    }

    /// The commit `word` names in a rebase: where a label was set, or
    /// else what the revision is.
    fn named_in(&self, state: &RebaseState, word: &str) -> Res<ObjectId> {
        if let Some((_, at)) = state.labels.iter().find(|label| label.0 == word) {
            return Ok(*at);
        }
        let named = self.store.git_view()?.rev_parse(word)?;
        named.ok_or_else(|| format!("rebase: '{word}' is no label set so far and no commit").into())
    }

    /// Replays `commit` onto where the rebase stands.
    fn replay(
        &mut self,
        state: &mut RebaseState,
        action: Action,
        commit: ObjectId,
        json: bool,
        out: &mut impl Write,
    ) -> Res<Option<bool>> {
        let head = self.rebase_head()?;
        if action == Action::Pick && !state.fresh_dates && self.stands_on(commit, head)? {
            self.save_rebase_state(state)?;
            self.step_onto(head, commit)?;
            return Ok(None);
        }
        // that it stopped at this step holds with the conflict it writes
        state.current = Some((action, commit));
        let stopped = state.text();
        state.current = None;
        match self.pick(head, commit, stopped)? {
            Picked::Conflicted(conflicts) => {
                let subject = self.read_commit_parts(commit)?.subject();
                let what = format!("{} {subject}", &commit.to_string()[..7]);
                self.report_rebase_stop(&what, &conflicts, json, out)?;
                return Ok(Some(true));
            }
            Picked::Clean(tree) if tree == self.commit_tree(head)? && !action.melds() => {
                // its changes are already in the new base: nothing left to replay
            }
            Picked::Clean(tree) => {
                // that the step is done holds with its commit
                self.save_rebase_state(state)?;
                self.apply_step(action, commit, tree, state.fresh_dates)?;
            }
        }
        Ok(None)
    }

    /// Makes the merge commit `commit` again: of where the rebase stands
    /// with what `brought` names, by its author and with its message.
    fn merge_again(
        &mut self,
        state: &mut RebaseState,
        commit: ObjectId,
        brought: &[String],
        json: bool,
        out: &mut impl Write,
    ) -> Res<Option<bool>> {
        let [word] = brought else {
            return Err("rebase: a merge of more than two commits is not made again".into());
        };
        let (head, theirs) = (self.rebase_head()?, self.named_in(state, word)?);
        if !state.fresh_dates && self.commit_parents(commit)? == [head, theirs] {
            self.save_rebase_state(state)?;
            self.step_onto(head, commit)?;
            return Ok(None);
        }
        let bases = self.merge_bases(&[head], &[theirs])?;
        if bases == [theirs] {
            // what it brought in is in already: git makes no merge of it
            return Ok(None);
        }
        let label = match state.labels.iter().any(|label| label.0 == *word) {
            true => format!("refs/rewritten/{word}"),
            false => word.clone(),
        };
        let base = self.base_entries(&bases)?;
        let (ours, theirs_now) = (self.commit_entries(head)?, self.commit_entries(theirs)?);
        let resolved = self.merge_trees(&base, &ours, &theirs_now, &label)?;
        self.store.odb.flush()?;
        if resolved.iter().any(|r| r.conflicted) {
            state.merging = Some((commit, theirs));
            self.plan_state("REBASE_STATE", Some(state.text()))?;
            self.write_conflicted(&resolved)?;
            let conflicted = resolved.iter().filter(|r| r.conflicted);
            let conflicts: Vec<BString> = conflicted.map(|r| r.path.clone()).collect();
            let subject = self.read_commit_parts(commit)?.subject();
            let what = format!("{}... {word} # {subject}", &commit.to_string()[..7]);
            self.report_rebase_stop(&what, &conflicts, json, out)?;
            return Ok(Some(true));
        }
        let entries: Vec<WorkEntry> = resolved.into_iter().filter_map(|r| r.entry).collect();
        let tree = self.store_tree(&entries)?;
        let old = index_entries(&self.index()?);
        self.checkout(&old, &entries)?;
        self.save_rebase_state(state)?;
        self.record_merge_again(commit, theirs, tree, state.fresh_dates)?;
        Ok(None)
    }

    /// Records the merge commit that stands for `commit`: `tree`, of where
    /// the rebase stands and `theirs`.
    pub(super) fn record_merge_again(
        &mut self,
        commit: ObjectId,
        theirs: ObjectId,
        tree: ObjectId,
        fresh_date: bool,
    ) -> Res<()> {
        let head = self.rebase_head()?;
        let merge = self.read_commit_parts(commit)?;
        let parents = [head, theirs];
        self.record_replayed(
            tree,
            &parents,
            &merge.author,
            &merge.message,
            commit,
            fresh_date,
        )
    }
}
