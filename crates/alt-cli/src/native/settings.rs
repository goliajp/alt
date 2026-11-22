//! The repository's git-style configuration, as kept with the store.

use super::*;
pub(super) use crate::git_settings::fatal;
use crate::git_settings::user_excludes;

/// The files outside the working tree that a scan of it goes by.
pub(super) struct ScanInputs {
    /// those that decide how a file compares, there or not
    pub(super) files: Vec<PathBuf>,
    /// the branch, where an `includeIf` of the configuration asks for
    /// it: the same files then make other settings on another branch
    pub(super) branch: Vec<u8>,
    /// the ignore files, a later one above an earlier
    pub(super) excludes: Vec<PathBuf>,
}

impl NativeRepo<'_> {
    pub(super) fn scan_inputs(&self) -> Res<ScanInputs> {
        let (settings, mut files) = self.settings_listed()?;
        files.extend(self.user_attributes(&settings)?);
        files.push(self.info_attributes());
        let by_branch = settings.entries.iter().any(|e| {
            let asks = |cond: &BString| cond.starts_with(b"onbranch:");
            e.section == "includeif" && e.subsection.as_ref().is_some_and(asks)
        });
        let branch = match by_branch {
            true => self.head_branch()?.into_bytes(),
            false => Vec::new(),
        };
        Ok(ScanInputs {
            files,
            branch,
            excludes: self.exclude_files(&settings)?,
        })
    }

    /// The ignore files outside the working tree, the one whose rules
    /// yield to the other's first: the user's, which `core.excludesFile`
    /// names, and the repository's own.
    pub(super) fn exclude_files(&self, settings: &alt_git_config::Config) -> Res<Vec<PathBuf>> {
        let user = user_excludes(settings)?.map(|named| self.root.join(named));
        let info = self.store.alt_dir.join("git-import/info/exclude");
        Ok(user.into_iter().chain([info]).collect())
    }

    /// Ends the command where a directory stands in place of an ignore
    /// file outside the working tree, with the words git has for it. git
    /// ends there every command that sets up its ignore rules, whether or
    /// not it then looks for untracked files.
    pub fn ensure_exclude_files(&self) -> Res<()> {
        self.ensure_exclude_files_of(&self.root)
    }

    /// [`ensure_exclude_files`](Self::ensure_exclude_files) for a tree of
    /// this repository at `root`, there or not.
    pub fn ensure_exclude_files_of(&self, root: &Path) -> Res<()> {
        let info = self.store.alt_dir.join("git-import/info/exclude");
        ensure_exclude_files(&self.settings()?, root, &info)
    }

    /// Ends a merge of `branch` where an ignore file outside the tree is
    /// a directory, as git ends one that has something to do: a
    /// fast-forward as it ends any checkout, a merge of two lines as it
    /// fails to put the state of the tree by. What else is wrong with the
    /// merge is the merge's to say.
    pub fn ensure_exclude_files_to_merge(&self, branch: &str) -> Res<()> {
        let Err(ended) = self.ensure_exclude_files() else {
            return Ok(());
        };
        let theirs = self.store.refs.resolve(&format!("refs/heads/{branch}"));
        let ours = self
            .head_branch()
            .and_then(|cur| Ok(self.store.refs.resolve(&cur)?));
        let (Ok(Some(ours)), Ok(Some(theirs))) = (ours, theirs) else {
            return Ok(());
        };
        let bases = self.merge_bases(&[ours], &[theirs])?;
        match (bases == [theirs], bases == [ours]) {
            (true, _) => Ok(()),
            (false, true) => Err(ended),
            (false, false) => Err(fatal(format!("{ended}\nfatal: stash failed"))),
        }
    }

    /// As `ensure_exclude_files`, for a cherry-pick of `rev`: a revision
    /// that names nothing is what git finds wrong first, and the command
    /// says so itself.
    pub fn ensure_exclude_files_to_pick(&self, rev: &str) -> Res<()> {
        let repo = alt_repo::Repository::discover(&self.store.alt_dir)?;
        match repo.rev_parse(rev)? {
            Some(_) => self.ensure_exclude_files(),
            None => Ok(()),
        }
    }

    /// The user's attributes file, which `core.attributesFile` names.
    pub(super) fn user_attributes(
        &self,
        settings: &alt_git_config::Config,
    ) -> Res<Option<PathBuf>> {
        crate::git_settings::user_attributes(settings, Some(&self.root))
    }

    /// The repository's own attributes file.
    pub(super) fn info_attributes(&self) -> PathBuf {
        self.store.alt_dir.join("git-import/info/attributes")
    }

    /// The configuration that applies in this repository, includes
    /// resolved: the system's, the user's, and on top the repository's
    /// own file.
    pub(super) fn settings(&self) -> Res<alt_git_config::Config> {
        Ok(self.settings_listed()?.0)
    }

    /// [`settings`](Self::settings), with the files they were read from or
    /// would be once those exist.
    fn settings_listed(&self) -> Res<(alt_git_config::Config, Vec<PathBuf>)> {
        let read = self.applying()?;
        Ok((read.config.clone(), read.files.clone()))
    }

    /// What [`settings`](Self::settings) are a copy of.
    pub(super) fn applying(&self) -> Res<std::sync::Arc<crate::git_settings::Applying>> {
        let dir = self.store.alt_dir.join("git-import");
        let branch = || {
            let full = self.head_branch().ok()?;
            let short = full.strip_prefix("refs/heads/")?;
            Some(BString::from(short.as_bytes().to_vec()))
        };
        crate::git_settings::applying(&dir, branch)
    }
}

/// Ends the command where a directory stands in place of an ignore file
/// outside the working tree at `root`: the user's, or `info`, which is the
/// repository's own.
pub(super) fn ensure_exclude_files(
    settings: &alt_git_config::Config,
    root: &Path,
    info: &Path,
) -> Res<()> {
    let info = info.strip_prefix(root).unwrap_or(info).to_owned();
    for named in user_excludes(settings)?.into_iter().chain([info]) {
        if root.join(&named).is_dir() {
            let named = named.display();
            return Err(fatal(format!("cannot use {named} as an exclude file")));
        }
    }
    Ok(())
}

/// What the system calls `error`, as git says it: without its number.
pub(super) fn reason(error: &std::io::Error) -> String {
    let said = error.to_string();
    match said.find(" (os error") {
        Some(at) => said[..at].to_owned(),
        None => said,
    }
}
