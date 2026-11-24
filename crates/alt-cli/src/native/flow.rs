//! `alt flow`: the git-flow commands, each one ref transaction.

use super::merge::MergeOutcome;
use super::*;
use crate::flow_args::Finish;

mod init;
mod tag;

impl NativeRepo<'_> {
    /// `alt flow feature start <name>`: branch `feature/<name>` off `develop`
    /// and switch to it, atomically (one op log entry → O(1) undo).
    pub fn flow_feature_start(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = self.flow_model().feature(name)?;
        let missing = "develop branch missing; run 'alt flow init' first";
        self.flow_topic_start(&flow, missing, json, out)
    }

    /// `alt flow feature finish <name>`: merge `feature/<name>` into `develop`
    /// with a merge commit, delete the feature branch, and move HEAD to
    /// `develop` — all in one atomic ref transaction (one op log entry →
    /// O(1) undo). Aborts if the merge conflicts.
    pub fn flow_feature_finish(
        &mut self,
        asked: &Finish,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let flow = self.flow_model().feature(&asked.name)?;
        let tag = self.finish_tag("feature", asked)?;
        let feat_ref = format!("refs/heads/{}", flow.branch);
        let dev_ref = format!("refs/heads/{}", flow.target);
        let feat = self
            .store
            .refs
            .resolve(&feat_ref)?
            .ok_or_else(|| format!("no such feature branch '{}'", flow.branch))?;
        let dev = self
            .store
            .refs
            .resolve(&dev_ref)?
            .ok_or("develop branch missing; run 'alt flow init' first")?;
        self.ensure_clean("flow finish")?;

        // merge the feature into develop without yet touching refs/work tree
        let (new_dev, entries) = self.flow_merge(dev, feat, &flow.branch, &flow.target, || {
            format!(
                "merge of '{}' into '{}' has conflicts; \
                 run 'alt merge' on '{}' and resolve manually",
                flow.branch, flow.target, flow.target
            )
        })?;

        // one atomic op: advance develop, delete the feature, move HEAD
        let head_old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        let old = index_entries(&self.index()?);
        let mut changes = vec![
            RefChange {
                name: feat_ref.clone(),
                old: Some(RefTarget::Oid(feat)),
                new: None,
            },
            RefChange {
                name: head_ref,
                old: head_old,
                new: Some(RefTarget::Symbolic(dev_ref.clone())),
            },
        ];
        // a feature without a commit of its own leaves develop where it is
        if new_dev != dev {
            changes.push(RefChange {
                name: dev_ref.clone(),
                old: Some(RefTarget::Oid(dev)),
                new: Some(RefTarget::Oid(new_dev)),
            });
        }
        changes.extend(self.finish_tagged(tag.as_ref(), new_dev)?);
        self.commit_refs_checkout("flow", &changes, &old, &entries)?;

        use crate::json::Json;
        let fields = vec![
            ("target", Json::str(&flow.target)),
            ("commit", Json::str(new_dev.to_string())),
            ("deleted", Json::str(&flow.branch)),
        ];
        let (topic, target) = (&flow.branch, &flow.target);
        let said = format!("Merged '{topic}' into '{target}' and deleted it");
        tag::finished(out, json, fields, tag, &said)
    }

    /// `alt flow release start <name>`: branch `release/<name>` off
    /// `develop` and switch to it — same atomic-ref-tx structure as
    /// feature start, just with a different base + branch prefix.
    pub fn flow_release_start(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = self.flow_model().release(name)?;
        let missing = format!(
            "base branch '{}' missing; run 'alt flow init' first",
            flow.base
        );
        self.flow_topic_start(&flow, &missing, json, out)
    }

    /// `alt flow release finish <name>`: merge `release/<name>` into
    /// `main` *and* back-merge `main` into `develop`, delete the release
    /// branch, move HEAD to `develop` — all in one ref-tx + one op-log
    /// entry. A conflict on either merge aborts the whole flow (atomicity
    /// keeps the prior state untouched).
    pub fn flow_release_finish(
        &mut self,
        asked: &Finish,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let flow = self.flow_model().release(&asked.name)?;
        let tag = self.finish_tag("release", asked)?;
        self.flow_topic_finish_dual(&flow, tag, "release", json, out)
    }

    /// `alt flow hotfix start <name>`: branch `hotfix/<name>` off `main`
    /// and switch to it.
    pub fn flow_hotfix_start(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = self.flow_model().hotfix(name)?;
        let missing = format!(
            "base branch '{}' missing; run 'alt flow init' first",
            flow.base
        );
        self.flow_topic_start(&flow, &missing, json, out)
    }

    /// `alt flow hotfix finish <name>`: merge `hotfix/<name>` into `main`
    /// and back-merge into `develop`, delete the hotfix branch, move HEAD
    /// to `develop`. Same atomic shape as release finish.
    pub fn flow_hotfix_finish(
        &mut self,
        asked: &Finish,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let flow = self.flow_model().hotfix(&asked.name)?;
        let tag = self.finish_tag("hotfix", asked)?;
        self.flow_topic_finish_dual(&flow, tag, "hotfix", json, out)
    }

    /// What a finish makes of `into`, at `ours`, when `theirs` is merged
    /// into it: the commit and its tree. Always a merge commit, as
    /// `git merge --no-ff` makes one, unless `ours` already has `theirs`.
    fn flow_merge(
        &mut self,
        ours: ObjectId,
        theirs: ObjectId,
        label: &str,
        into: &str,
        conflicts: impl FnOnce() -> String,
    ) -> Res<(ObjectId, Vec<WorkEntry>)> {
        match self.compute_merge(ours, theirs, label, into, FastForward::Never)? {
            MergeOutcome::UpToDate => Ok((ours, self.commit_entries(ours)?)),
            MergeOutcome::FastForward(commit) => Ok((commit, self.commit_entries(commit)?)),
            MergeOutcome::Merged { commit, entries } => Ok((commit, entries)),
            MergeOutcome::Conflicted(_) => Err(conflicts().into()),
        }
    }

    /// Shared start path for any flow whose start = "branch <topic> off
    /// <base>, switch HEAD to topic". Single ref-tx, one op-log entry.
    /// `missing` is what it says where the base branch is not there.
    fn flow_topic_start(
        &mut self,
        flow: &alt_flow::Flow,
        missing: &str,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let topic_ref = format!("refs/heads/{}", flow.branch);
        let base_ref = format!("refs/heads/{}", flow.base);
        if self.store.refs.get(&topic_ref).is_some() {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                format!("a branch named '{}' already exists", flow.branch),
            )
            .into());
        }
        let base = self.store.refs.resolve(&base_ref)?.ok_or(missing)?;
        self.ensure_clean("flow start")?;

        let target = self.commit_entries(base)?;
        let old = index_entries(&self.index()?);

        let head_old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        self.commit_refs_checkout(
            "flow",
            &[
                RefChange {
                    name: topic_ref.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(base)),
                },
                RefChange {
                    name: head_ref,
                    old: head_old,
                    new: Some(RefTarget::Symbolic(topic_ref.clone())),
                },
            ],
            &old,
            &target,
        )?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("branch", Json::str(&flow.branch)),
                    ("base", Json::str(&flow.base)),
                    ("commit", Json::str(base.to_string())),
                ],
            )?;
        } else {
            writeln!(out, "Switched to a new branch '{}'", flow.branch)?;
        }
        Ok(())
    }

    /// Shared dual-target finish: merge `flow.branch` into `flow.target`
    /// AND back-merge `flow.target` into `back_ref` (develop, by
    /// convention), delete the topic, move HEAD to `back_ref`. All in
    /// one ref-tx. A conflict on either merge aborts.
    fn flow_topic_finish_dual(
        &mut self,
        flow: &alt_flow::Flow,
        tag: Option<tag::FinishTag>,
        verb: &str,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let back_ref = "refs/heads/develop";
        let topic_ref = format!("refs/heads/{}", flow.branch);
        let target_ref = format!("refs/heads/{}", flow.target);
        let topic_oid = self
            .store
            .refs
            .resolve(&topic_ref)?
            .ok_or_else(|| format!("no such {verb} branch '{}'", flow.branch))?;
        let target_oid = self
            .store
            .refs
            .resolve(&target_ref)?
            .ok_or_else(|| format!("{} branch missing", flow.target))?;
        let back_oid = self
            .store
            .refs
            .resolve(back_ref)?
            .ok_or_else(|| format!("{back_ref} missing; run 'alt flow init' first"))?;
        self.ensure_clean("flow finish")?;

        // Step 1: merge the topic into its target (main, typically). A
        // conflict here aborts without touching any ref.
        let (new_target, target_entries) =
            self.flow_merge(target_oid, topic_oid, &flow.branch, &flow.target, || {
                format!(
                    "merge of '{}' into '{}' has conflicts; \
                     resolve manually before retrying the {verb} finish",
                    flow.branch, flow.target
                )
            })?;

        // Step 2: back-merge the (just-advanced) target into develop, so
        // hotfixes/releases land on both main and develop. Conflict here
        // aborts the whole flow — the prior state is untouched until the
        // single ref-tx below commits.
        let back = back_ref.trim_start_matches("refs/heads/");
        let (new_back, back_entries) = if new_target == back_oid {
            // back already points at the same commit (rare but possible
            // when develop == main): no second merge to compute
            (back_oid, target_entries.clone())
        } else {
            self.flow_merge(back_oid, new_target, &flow.target, back, || {
                format!(
                    "back-merge of '{}' into '{back_ref}' has conflicts; \
                     resolve manually before retrying the {verb} finish",
                    flow.target
                )
            })?
        };

        // Step 3: one atomic ref-tx — advance target + advance back-ref +
        // delete topic + move HEAD to back-ref. SIGKILL anywhere splits
        // into pre/post only.
        let head_old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        let mut changes = vec![
            RefChange {
                name: topic_ref.clone(),
                old: Some(RefTarget::Oid(topic_oid)),
                new: None,
            },
            RefChange {
                name: head_ref,
                old: head_old,
                new: Some(RefTarget::Symbolic(back_ref.to_owned())),
            },
        ];
        // a branch is in the transaction only when it moved: a RefChange
        // with old == new is one the refs layer rejects
        let moved = [
            (target_ref.as_str(), target_oid, new_target),
            (back_ref, back_oid, new_back),
        ];
        for (name, old, new) in moved.into_iter().filter(|(_, old, new)| old != new) {
            changes.push(RefChange {
                name: name.to_owned(),
                old: Some(RefTarget::Oid(old)),
                new: Some(RefTarget::Oid(new)),
            });
        }
        changes.extend(self.finish_tagged(tag.as_ref(), new_target)?);
        let old = index_entries(&self.index()?);
        self.commit_refs_checkout("flow", &changes, &old, &back_entries)?;
        use crate::json::Json;
        let fields = vec![
            ("kind", Json::str(verb)),
            ("target", Json::str(&flow.target)),
            ("target_commit", Json::str(new_target.to_string())),
            ("back", Json::str(back)),
            ("back_commit", Json::str(new_back.to_string())),
            ("deleted", Json::str(&flow.branch)),
        ];
        let (topic, target) = (&flow.branch, &flow.target);
        let said = format!(
            "Merged '{topic}' into '{target}' and back-merged into '{back_ref}', deleted '{topic}'"
        );
        tag::finished(out, json, fields, tag, &said)
    }
}
