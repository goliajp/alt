//! `alt merge`: working out how two commits merge, and bringing the
//! result onto the current branch.

use super::*;

impl NativeRepo<'_> {
    /// `alt merge <branch>`: three-way merge `branch` into the current branch.
    /// Returns whether the merge stopped in conflict. Fast-forwards when our
    /// branch is strictly behind and `ff` lets it; otherwise writes a
    /// two-parent merge commit (clean) or leaves conflict markers + unmerged
    /// index entries and makes no commit (conflicting). A clean working tree
    /// is required.
    pub fn merge(
        &mut self,
        branch_name: &str,
        ff: FastForward,
        json: bool,
        out: &mut impl Write,
    ) -> Res<bool> {
        self.ensure_topic_branch_or_unborn("merge")?;
        let their_ref = format!("refs/heads/{branch_name}");
        let theirs = self
            .store
            .refs
            .resolve(&their_ref)?
            .ok_or_else(|| format!("merge: {branch_name} - not something we can merge"))?;
        let cur = self.head_branch()?;
        let ours = self
            .store
            .refs
            .resolve(&cur)?
            .ok_or("cannot merge into an unborn branch")?;
        self.ensure_clean("merge")?;

        let into = cur.strip_prefix("refs/heads/").unwrap_or(&cur).to_owned();
        match self.compute_merge(ours, theirs, branch_name, &into, ff)? {
            MergeOutcome::UpToDate => {
                self.report_merge(json, out, "up_to_date", None, &[], "Already up to date.")?;
                Ok(false)
            }
            MergeOutcome::FastForward(commit) => {
                let target = self.commit_entries(commit)?;
                let old = index_entries(&self.index()?);
                self.advance_branch_checkout(&cur, ours, commit, &old, &target)?;
                self.report_merge(
                    json,
                    out,
                    "fast_forward",
                    Some(commit),
                    &[],
                    &format!("Fast-forward to {commit}"),
                )?;
                Ok(false)
            }
            MergeOutcome::Merged { commit, entries } => {
                let old = index_entries(&self.index()?);
                self.advance_branch_checkout(&cur, ours, commit, &old, &entries)?;
                self.report_merge(
                    json,
                    out,
                    "merged",
                    Some(commit),
                    &[],
                    "Merge made by the 'ort' strategy.",
                )?;
                Ok(false)
            }
            MergeOutcome::Conflicted(resolved) => {
                self.start_merge(theirs)?;
                self.write_conflicted(&resolved)?;
                self.say_stopped(&resolved, json, out)?;
                Ok(true)
            }
        }
    }

    /// What a merge says that stopped in conflict at paths of `resolved`.
    fn say_stopped(&self, resolved: &[Resolved], json: bool, out: &mut impl Write) -> Res<()> {
        let conflicted = resolved.iter().filter(|r| r.conflicted);
        let conflicts: Vec<BString> = conflicted.map(|r| r.path.clone()).collect();
        if json {
            return self.report_merge(true, out, "conflicted", None, &conflicts, "");
        }
        cherry_pick::say_conflicts(out, "Merge", &conflicts)?;
        let fix = "Automatic merge failed; fix conflicts and then commit the result.";
        Ok(writeln!(out, "{fix}")?)
    }

    /// Emits one merge outcome: the human line, or `{schema_version, result,
    /// commit?, conflicts}` for `--json`, with `conflicts_bytes` where a
    /// conflicted path is not UTF-8.
    pub(in crate::native) fn report_merge(
        &self,
        json: bool,
        out: &mut impl Write,
        result: &'static str,
        commit: Option<ObjectId>,
        conflicts: &[BString],
        human: &str,
    ) -> Res<()> {
        if json {
            use crate::json::Json;
            let commit = commit.map_or(Json::Null, |c| Json::str(c.to_string()));
            let mut fields = vec![("result", Json::str(result)), ("commit", commit)];
            let conflicts = conflicts.iter().map(|path| path.as_slice());
            fields.extend(crate::json::paths(
                "conflicts",
                "conflicts_bytes",
                conflicts,
            ));
            crate::json::emit(out, fields)?;
        } else {
            writeln!(out, "{human}")?;
        }
        Ok(())
    }

    /// Computes how merging `theirs` into `ours` resolves, writing any merged
    /// blobs / tree / merge commit to the object store but touching no refs or
    /// working tree. The caller applies the outcome (advancing refs, checking
    /// out, or recording a conflict) — which lets both `alt merge` and the
    /// atomic `flow finish` share one merge engine. `into` is the short name
    /// of the branch merged into, which the message of a merge commit names
    /// as git does.
    pub(in crate::native) fn compute_merge(
        &mut self,
        ours: ObjectId,
        theirs: ObjectId,
        label: &str,
        into: &str,
        ff: FastForward,
    ) -> Res<MergeOutcome> {
        let bases = self.merge_bases(&[ours], &[theirs])?;
        if bases == [theirs] {
            return Ok(MergeOutcome::UpToDate);
        }
        if bases == [ours] {
            if ff != FastForward::Never {
                return Ok(MergeOutcome::FastForward(theirs));
            }
            self.ensure_signing()?;
            // the tree of a merge that could have been a fast-forward is theirs
            let entries = self.commit_entries(theirs)?;
            let commit = self.write_merge_commit(ours, theirs, &entries, label, into)?;
            return Ok(MergeOutcome::Merged { commit, entries });
        }
        if ff == FastForward::Only {
            return Err(settings::fatal(
                "Not possible to fast-forward, aborting.".into(),
            ));
        }
        // nobody to make the commit is found before anything is stored
        self.ensure_signing()?;
        let base_entries = self.base_entries(&bases)?;
        let ours_entries = self.commit_entries(ours)?;
        let theirs_entries = self.commit_entries(theirs)?;
        let resolved = self.merge_trees(&base_entries, &ours_entries, &theirs_entries, label)?;
        self.store.odb.flush()?;

        if resolved.iter().any(|r| r.conflicted) {
            return Ok(MergeOutcome::Conflicted(resolved));
        }

        let entries: Vec<WorkEntry> = resolved.iter().filter_map(|r| r.entry.clone()).collect();
        let commit = self.write_merge_commit(ours, theirs, &entries, label, into)?;
        Ok(MergeOutcome::Merged { commit, entries })
    }

    /// Stores the commit that merges `theirs` into `ours` with the tree of
    /// `entries`. Its message is git's: the branch merged into is named
    /// unless it is `main` or `master`.
    fn write_merge_commit(
        &mut self,
        ours: ObjectId,
        theirs: ObjectId,
        entries: &[WorkEntry],
        label: &str,
        into: &str,
    ) -> Res<ObjectId> {
        let (author, committer) = (self.author()?, self.committer()?);
        let algo = self.store.algo()?;
        let tree = write_tree(&mut self.store.odb, entries, algo)?;
        let msg = match into {
            "main" | "master" => format!("Merge branch '{label}'\n"),
            _ => format!("Merge branch '{label}' into {into}\n"),
        };
        let commit = write_commit(
            &mut self.store.odb,
            tree,
            &[ours, theirs],
            &author.sig(),
            &committer.sig(),
            &msg,
            algo,
        )?;
        self.store.odb.flush()?;
        Ok(commit)
    }
}

/// Whether a merge may end as a fast-forward, as git's `--ff`, `--no-ff`
/// and `--ff-only` say.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FastForward {
    /// fast-forward where the branch is behind, else a merge commit
    Allowed,
    /// a merge commit even where the branch is behind
    Never,
    /// a fast-forward, or nothing
    Only,
}

/// How a merge resolves once computed (no refs or work tree touched yet).
pub(in crate::native) enum MergeOutcome {
    /// `theirs` is already an ancestor of `ours`; nothing to do.
    UpToDate,
    /// `ours` is an ancestor of `theirs`; advance to this commit.
    FastForward(ObjectId),
    /// A clean three-way merge: this merge commit and its flattened tree.
    Merged {
        commit: ObjectId,
        entries: Vec<WorkEntry>,
    },
    /// At least one path conflicts; the per-path resolutions to write out.
    Conflicted(Vec<Resolved>),
}
