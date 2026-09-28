//! Recording commits: `alt commit` and `alt commit --amend`.

use super::*;

impl NativeRepo<'_> {
    /// `alt commit -m <msg>`: write a tree + commit from the index, advance
    /// the current branch in one ref transaction.
    pub fn commit(
        &mut self,
        message: &str,
        allow_empty: bool,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("commit")?;
        self.ensure_topic_branch_or_unborn("commit")?;
        // Path gate is `add`-only on purpose: the restricted principal's
        // *choice* of what to stage is what the policy constrains. Pre-existing
        // index entries inherited from another principal (e.g. operator's
        // baseline) ride through to the commit unchallenged — penalising the
        // agent for paths it never touched is unhelpful.
        let tree = self.staged_tree()?;

        let branch = self.head_branch()?;
        let parent = self.store.refs.resolve(&branch)?;
        let merging = self.merge_head()?;
        // a commit that changes nothing is almost always a mistake (a
        // forgotten `add`); git refuses it too, merges aside
        if let Some(p) = parent
            && merging.is_none()
            && !allow_empty
            && self.commit_tree(p)? == tree
        {
            return Err(
                "nothing to commit: the index matches HEAD (use --allow-empty to \
                        record an empty commit)"
                    .into(),
            );
        }
        let parents: Vec<ObjectId> = parent.into_iter().chain(merging).collect();

        let id = self.id.clone();
        let (name, email) = id.sig();
        let me = Sig {
            name,
            email,
            when: (now_ms() / 1000) as i64,
            tz: "+0000",
        };
        let msg = with_newline(message);
        let commit = self.record_commit(
            NewCommit {
                tree,
                parents: &parents,
                author: &me,
                committer: &me,
                message: &msg,
            },
            &branch,
            parent,
            "commit",
        )?;
        if merging.is_some() {
            self.finish_merge()?;
        }
        self.revert_committed()?;
        self.rebase_committed()?;
        report_commit(&branch, commit, tree, json, out)
    }

    /// `alt commit --amend [-m <msg>]`: replace the branch tip with a new
    /// commit built from the index, as git does — same parents and author,
    /// the new (or, without `-m`, the old) message, a fresh committer. The
    /// old commit is left untouched and stays reachable through the op log.
    pub fn amend(&mut self, message: Option<&str>, json: bool, out: &mut impl Write) -> Res<()> {
        self.ensure_writable("commit --amend")?;
        self.ensure_topic_branch_or_unborn("commit --amend")?;
        self.ensure_no_merge_in_progress("commit --amend")?;
        let branch = self.head_branch()?;
        let old = self
            .store
            .refs
            .resolve(&branch)?
            .ok_or("commit --amend: there is no commit to amend yet")?;
        let obj = self
            .store
            .odb
            .get(&old)?
            .ok_or("commit missing from store")?;
        let prev = alt_git_codec::Commit::parse(&obj.data)?;
        let parents: Vec<ObjectId> = prev.parents().collect();
        let author_line = prev
            .author()
            .ok_or("commit --amend: the commit has no author")?
            .to_str()
            .map_err(|_| "commit --amend: the author is not UTF-8")?
            .to_owned();
        let author = parse_sig(&author_line)?;
        let msg = match message {
            Some(m) => with_newline(m),
            None => prev
                .message()
                .to_str()
                .map_err(|_| "commit --amend: the message is not UTF-8; pass -m")?
                .to_owned(),
        };

        let tree = self.staged_tree()?;
        let id = self.id.clone();
        let (name, email) = id.sig();
        let me = Sig {
            name,
            email,
            when: (now_ms() / 1000) as i64,
            tz: "+0000",
        };
        let commit = self.record_commit(
            NewCommit {
                tree,
                parents: &parents,
                author: &author,
                committer: &me,
                message: &msg,
            },
            &branch,
            Some(old),
            "amend",
        )?;
        report_commit(&branch, commit, tree, json, out)
    }

    /// The index's stage-0 entries written out as a tree.
    pub(super) fn staged_tree(&mut self) -> Res<ObjectId> {
        let staged = index_entries(&self.index()?);
        if staged.is_empty() {
            return Err("nothing to commit (empty index)".into());
        }
        Ok(write_tree(&mut self.store.odb, &staged, self.store.algo)?)
    }

    /// Stores a commit (signed when the sign policy asks for it) and moves
    /// `branch` from `old` to it in one ref transaction.
    pub(super) fn record_commit(
        &mut self,
        c: NewCommit<'_>,
        branch: &str,
        old: Option<ObjectId>,
        verb: &str,
    ) -> Res<ObjectId> {
        let mut bytes = build_commit_bytes(c.tree, c.parents, c.author, c.committer, c.message);
        // when sign-policy is on and a sec key is on disk for
        // the principal, splice an `alt-sig` header into the commit and
        // rehash. The signed commit is the canonical commit from the
        // store's POV — there is no second "unsigned" stored.
        if let Some(signed) = self.maybe_sign_commit_bytes(&bytes)? {
            bytes = signed;
        }
        let commit = ObjectId::hash_object(self.store.algo, ObjectKind::Commit, &bytes);
        self.store.odb.put(commit, ObjectKind::Commit, &bytes)?;
        self.store.odb.flush()?;
        self.commit_refs(
            verb,
            &[RefChange {
                name: branch.to_owned(),
                old: old.map(RefTarget::Oid),
                new: Some(RefTarget::Oid(commit)),
            }],
        )?;
        Ok(commit)
    }
}

/// The content of a commit about to be written.
pub(super) struct NewCommit<'a> {
    pub(super) tree: ObjectId,
    pub(super) parents: &'a [ObjectId],
    pub(super) author: &'a Sig<'a>,
    pub(super) committer: &'a Sig<'a>,
    pub(super) message: &'a str,
}

fn with_newline(message: &str) -> String {
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

fn report_commit(
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
