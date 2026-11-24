//! `alt commit --amend`.

use super::*;

impl NativeRepo<'_> {
    /// Refuses an amend while another operation waits to be finished. A
    /// rebase that waits between two of its steps is none: the commit
    /// it made last is amended, as by git.
    fn ensure_amendable(&self) -> Res<()> {
        match self.rebase_state()? {
            Some(state) if !state.stopped() => Ok(()),
            _ => self.ensure_no_merge_in_progress("commit --amend"),
        }
    }

    /// The tree of the index for a commit that takes the place of one
    /// with `parents` and the tree `base`. A commit amended into one that
    /// changes nothing is refused as by git, a merge aside, unless
    /// `allow_empty` asks for it.
    fn amended_tree(
        &mut self,
        parents: &[ObjectId],
        base: Option<ObjectId>,
        allow_empty: bool,
    ) -> Res<ObjectId> {
        let nothing = || {
            let said = "You asked to amend the most recent commit, but doing so would make\n\
                        it empty. You can repeat your command with --allow-empty, or you can\n\
                        remove the commit entirely with \"git reset HEAD^\".\n";
            said::refused(said, 1)
        };
        if parents.is_empty() && !allow_empty && self.index()?.entries.is_empty() {
            return Err(nothing());
        }
        let tree = self.staged_tree(base)?;
        let before = match parents[..] {
            [parent] => Some(self.commit_tree(parent)?),
            _ => None,
        };
        if before == Some(tree) && !allow_empty {
            return Err(nothing());
        }
        Ok(tree)
    }

    /// `alt commit --amend [-m <msg>]`: replace the branch tip with a new
    /// commit built from the index, as git does — same parents and author,
    /// the new (or, without `-m`, the old) message, a fresh committer. The
    /// old commit is left untouched and stays reachable through the op log.
    pub fn amend(
        &mut self,
        message: Option<&str>,
        opts: CommitOptions,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("commit --amend")?;
        self.ensure_topic_branch_or_unborn("commit --amend")?;
        self.ensure_amendable()?;
        let committer = self.committer()?;
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
        let author = self.authored(&opts.by, Some(&parse_sig(&author_line)?))?;
        let msg = match message {
            Some(m) => with_newline(m),
            None => prev
                .message()
                .to_str()
                .map_err(|_| "commit --amend: the message is not UTF-8; pass -m")?
                .to_owned(),
        };

        let tree = self.amended_tree(&parents, prev.tree(), opts.allow_empty)?;
        if !opts.no_verify {
            self.run_precommit()?;
        }
        let commit = self.record_commit(
            NewCommit {
                effects: recovery::RefEffects::RefsOnly,
                tree,
                parents: &parents,
                author: &author.sig(),
                committer: &committer.sig(),
                message: &msg,
            },
            &branch,
            Some(old),
            "amend",
            &opts.meta(Some(old)),
        )?;
        report_commit(&branch, commit, tree, json, out)
    }
}
