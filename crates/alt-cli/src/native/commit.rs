//! Recording commits: `alt commit` and `alt commit --amend`.

use super::notes::MetaInput;
use super::*;
use crate::precommit::{self, Change, Severity};

mod amend;
mod authored;
pub use authored::Authored;

/// How `alt commit` was asked to behave.
#[derive(Debug, Default, Clone)]
pub struct CommitOptions {
    /// Record the commit even when it changes nothing.
    pub allow_empty: bool,
    /// Skip the pre-commit checks.
    pub no_verify: bool,
    /// One-line decisions to record in the commit's metadata.
    pub decisions: Vec<String>,
    /// Extra `key=value` metadata fields.
    pub extra: Vec<(String, String)>,
    /// What the command line says of the author.
    pub by: Authored,
}

impl CommitOptions {
    pub fn of(args: &crate::commit_args::Commit) -> Self {
        CommitOptions {
            allow_empty: args.allow_empty,
            no_verify: args.no_verify,
            decisions: args.decisions.clone(),
            extra: args.extra.clone(),
            by: Authored {
                author: args.author_given.clone(),
                date: args.author_date.clone(),
                reset: args.reset_author,
            },
        }
    }

    fn meta(&self, carry_from: Option<ObjectId>) -> MetaInput {
        MetaInput {
            decisions: self.decisions.clone(),
            extra: self.extra.clone(),
            carry_from,
        }
    }
}

impl NativeRepo<'_> {
    /// `alt commit -m <msg>`: write a tree + commit from the index, advance
    /// the current branch in one ref transaction.
    pub fn commit(
        &mut self,
        message: &str,
        opts: CommitOptions,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("commit")?;
        self.ensure_topic_branch_or_unborn("commit")?;
        self.ensure_committable()?;
        if opts.by.reset {
            let said = "--reset-author can be used only with -C, -c or --amend.";
            return Err(settings::fatal(said.to_owned()));
        }
        let committer = self.committer()?;
        // nobody to name as the author ends the command before it stores
        if self.picked_author()?.is_none() {
            self.authored(&opts.by, None)?;
        }
        let branch = self.head_branch()?;
        let parent = self.store.refs.resolve(&branch)?;
        let head_tree = parent.map(|p| self.commit_tree(p)).transpose()?;
        let tree = self.staged_tree(head_tree)?;
        let merging = self.merge_head()?;
        // a commit that changes nothing is almost always a mistake (a
        // forgotten `add`); git refuses it too, merges aside
        if head_tree == Some(tree) && merging.is_none() && !opts.allow_empty {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "nothing to commit: the index matches HEAD (use --allow-empty to \
                        record an empty commit)",
            )
            .into());
        }
        if !opts.no_verify {
            self.run_precommit()?;
        }
        let parents: Vec<ObjectId> = parent.into_iter().chain(merging).collect();

        let me = committer.sig();
        let msg = with_newline(message);
        let picked = self.concluding()?;
        let by = self.authored(&opts.by, picked.as_ref())?;
        let commit = self.record_commit(
            NewCommit {
                effects: recovery::RefEffects::RefsOnly,
                tree,
                parents: &parents,
                author: &by.sig(),
                committer: &me,
                message: &msg,
            },
            &branch,
            parent,
            "commit",
            &opts.meta(None),
        )?;
        self.settle_state()?;
        report_commit(&branch, commit, tree, json, out)
    }

    /// `alt commit --validate`: run the pre-commit checks on what is staged
    /// without committing. Returns whether any check failed.
    pub fn validate(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        let findings = self.precommit_findings()?;
        if json {
            crate::json::emit(out, precommit::to_json(&findings))?;
        } else if findings.is_empty() {
            writeln!(out, "pre-commit checks passed")?;
        } else {
            write!(out, "{}", precommit::render(&findings))?;
        }
        Ok(findings.iter().any(|f| f.severity == Severity::Error))
    }

    /// Runs the pre-commit checks: warnings go to stderr, errors stop the
    /// commit.
    pub(super) fn run_precommit(&mut self) -> Res<()> {
        let (errors, warnings): (Vec<_>, Vec<_>) = self
            .precommit_findings()?
            .into_iter()
            .partition(|f| f.severity == Severity::Error);
        crate::diagnostics::emit(precommit::render(&warnings).as_bytes())?;
        if !errors.is_empty() {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                format!(
                    "commit refused by the pre-commit checks (see `alt commit --validate --json`; \
                 skip them with --no-verify):\n{}",
                    precommit::render(&errors).trim_end()
                ),
            )
            .into());
        }
        Ok(())
    }

    /// Checks every file the index changes against HEAD.
    fn precommit_findings(&mut self) -> Res<Vec<precommit::Finding>> {
        let head: std::collections::HashMap<BString, WorkEntry> = self
            .head_entries()?
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect();
        let mut files = Vec::new();
        for e in index_entries(&self.index()?) {
            // links and submodules carry no content of their own to scan
            if e.mode == 0o120000 || e.mode == 0o160000 {
                continue;
            }
            let old = head.get(&e.path);
            if old.is_some_and(|o| o.oid == e.oid) {
                continue;
            }
            // what a submodule entry names is a commit of another store
            let old = old.filter(|o| o.mode != 0o160000);
            let old = old.map(|o| self.blob_bytes(o.oid)).transpose()?;
            files.push((e.path.to_string(), old, self.blob_bytes(e.oid)?));
        }
        let changes: Vec<Change> = files
            .iter()
            .map(|(path, old, new)| Change {
                path,
                old: old.as_deref(),
                new,
            })
            .collect();
        Ok(precommit::check(&changes))
    }

    /// The index's stage-0 entries written out as a tree. Refused while a
    /// conflict is unresolved: a tree without its path would delete the file.
    /// The blob of every entry that `base`, the tree of a commit, does not
    /// have is looked up first: the index can name one that was never stored.
    pub(super) fn staged_tree(&mut self, base: Option<ObjectId>) -> Res<ObjectId> {
        let index = self.index()?;
        if index.entries.iter().any(|e| e.stage() > 0) {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "unmerged paths remain: resolve the conflicts and `alt add` them first",
            )
            .into());
        }
        let staged = index_entries(&index);
        if staged.is_empty() {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "nothing to commit (empty index)",
            )
            .into());
        }
        let tree = self.store_tree(&staged)?;
        let algo = self.store.algo()?;
        if let Some((path, blob)) = alt_worktree::unstored_blob(&self.store.odb, tree, base, algo)?
        {
            return Err(format!(
                "index entry {path} names blob {blob}, which is not stored: add the file again"
            )
            .into());
        }
        Ok(tree)
    }

    /// Stores a commit (signed when the sign policy asks for it) and moves
    /// `branch` from `old` to it, together with the commit's metadata note,
    /// in one ref transaction.
    pub(super) fn record_commit(
        &mut self,
        c: NewCommit<'_>,
        branch: &str,
        old: Option<ObjectId>,
        verb: &str,
        meta: &MetaInput,
    ) -> Res<ObjectId> {
        self.ensure_commit_allowed(old, c.tree)?;
        let mut bytes = build_commit_bytes(c.tree, c.parents, c.author, c.committer, c.message);
        // when sign-policy is on and a sec key is on disk for
        // the principal, splice an `alt-sig` header into the commit and
        // rehash. The signed commit is the canonical commit from the
        // store's POV — there is no second "unsigned" stored.
        if let Some(signed) = self.maybe_sign_commit_bytes(&bytes)? {
            bytes = signed;
        }
        let commit = ObjectId::hash_object(self.store.algo()?, ObjectKind::Commit, &bytes);
        let note = self.meta_for(c.message, c.tree, c.parents, meta)?;
        let move_branch = RefChange {
            name: branch.to_owned(),
            old: old.map(RefTarget::Oid),
            new: Some(RefTarget::Oid(commit)),
        };
        let of = (commit, Some(&bytes[..]));
        let operation = self.commit_refs_with_note(verb, &[move_branch], of, &note, c.effects)?;
        if let Some(intent) = self.checkout_intent.clone() {
            self.apply_prepared_checkout(operation, &intent)?;
        }
        Ok(commit)
    }
}

/// The content of a commit about to be written.
pub(super) struct NewCommit<'a> {
    pub(super) effects: recovery::RefEffects,
    pub(super) tree: ObjectId,
    pub(super) parents: &'a [ObjectId],
    pub(super) author: &'a Sig<'a>,
    pub(super) committer: &'a Sig<'a>,
    pub(super) message: &'a str,
}

pub(super) fn with_newline(message: &str) -> String {
    if message.ends_with('\n') {
        message.to_owned()
    } else {
        format!("{message}\n")
    }
}

/// Splits a git identity line `Name <email> <seconds> <tz>`.
pub(super) fn parse_sig(line: &str) -> Res<Sig<'_>> {
    let bad = || format!("malformed identity line: {line}");
    let (who, tz) = line.rsplit_once(' ').ok_or_else(bad)?;
    let (who, when) = who.rsplit_once(' ').ok_or_else(bad)?;
    let (name, email) = who
        .strip_suffix('>')
        .and_then(|w| w.rsplit_once(" <"))
        .ok_or_else(bad)?;
    Ok(Sig {
        name,
        email,
        when: when.parse().map_err(|_| bad())?,
        tz,
    })
}

pub(super) fn report_commit(
    branch: &str,
    commit: ObjectId,
    tree: ObjectId,
    json: bool,
    out: &mut impl Write,
) -> Res<()> {
    let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    if json {
        use crate::json::Json;
        crate::json::emit(
            out,
            vec![
                ("branch", Json::str(short)),
                ("commit", Json::str(commit.to_string())),
                ("tree", Json::str(tree.to_string())),
            ],
        )?;
    } else {
        writeln!(out, "[{short}] {commit}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_lines_split_like_git() {
        let s = parse_sig("A U Thor <a@b.c> 1700000000 +0900").unwrap();
        assert_eq!(
            (s.name, s.email, s.when, s.tz),
            ("A U Thor", "a@b.c", 1_700_000_000, "+0900")
        );
        // an email may hold spaces before the closing bracket; the last " <" splits
        let s = parse_sig("x <y z> 5 -0100").unwrap();
        assert_eq!((s.name, s.email), ("x", "y z"));
        assert!(parse_sig("no identity here").is_err());
        assert!(parse_sig("x <y> notanumber +0000").is_err());
    }
}
