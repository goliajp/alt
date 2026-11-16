//! The repository's git-style configuration, as kept with the store.

use super::*;

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
        let (global, info) = self.attribute_files(&settings)?;
        files.extend(global);
        files.push(info);
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

    /// The attribute files outside the working tree: the user's, which
    /// `core.attributesFile` names, and the repository's own.
    pub(super) fn attribute_files(
        &self,
        settings: &alt_git_config::Config,
    ) -> Res<(Option<PathBuf>, PathBuf)> {
        let user = match named_file(settings, "attributesfile")? {
            Some(named) => named.map(|named| self.root.join(named)),
            None => std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .map(|config| config.join("git").join("attributes")),
        };
        let info = self.store.alt_dir.join("git-import/info/attributes");
        Ok((user, info))
    }

    /// The configuration file of this repository with its includes
    /// resolved; empty when the repository has none.
    pub(super) fn settings(&self) -> Res<alt_git_config::Config> {
        Ok(self.settings_listed()?.0)
    }

    /// [`settings`](Self::settings), with the files they were read from or
    /// would be once those exist.
    fn settings_listed(&self) -> Res<(alt_git_config::Config, Vec<PathBuf>)> {
        let dir = self.store.alt_dir.join("git-import");
        let file = dir.join("config");
        if !file.is_file() {
            return Ok((alt_git_config::Config::default(), vec![file]));
        }
        let branch = self.head_branch().ok().and_then(|full| {
            let short = full.strip_prefix("refs/heads/")?;
            Some(BString::from(short.as_bytes().to_vec()))
        });
        let ctx = alt_git_config::IncludeContext {
            git_dir: Some(dir),
            branch,
            home: std::env::var_os("HOME").map(PathBuf::from),
        };
        Ok(alt_git_config::Config::load_listing(&file, &ctx)?)
    }
}

/// The user's ignore file as it is named: a relative path is one in the
/// working tree.
fn user_excludes(settings: &alt_git_config::Config) -> Res<Option<PathBuf>> {
    match named_file(settings, "excludesfile")? {
        Some(named) => Ok(named),
        None => Ok(alt_worktree::default_excludes_file()),
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

/// The file the setting `key` of the `core` section names, as git
/// reads it: `None` when it is not set, no file when it is set to
/// nothing. A relative path is one in the working tree. A key without
/// a value is an error, and so is a home directory that is not known.
fn named_file(settings: &alt_git_config::Config, key: &str) -> Res<Option<Option<PathBuf>>> {
    match settings.get("core", None, key) {
        None => Ok(None),
        Some(None) => {
            let missing = format!("error: missing value for 'core.{key}'\n");
            crate::diagnostics::emit(missing.as_bytes())?;
            Err(fatal(format!("bad config variable 'core.{key}'")))
        }
        Some(Some(path)) if path.is_empty() => Ok(Some(None)),
        Some(Some(path)) => Ok(Some(Some(expand_user(path)?))),
    }
}

/// A failure that changed nothing, with the exit status git has for it.
pub(super) fn fatal(message: String) -> Box<dyn std::error::Error> {
    let refusal = crate::machine::Failure::refusal("execution_error", message);
    Box::new(crate::machine::Failure {
        exit_code: 128,
        ..refusal
    })
}

/// A path from the configuration, as git expands it: `~/` stands for the
/// home directory, `~name/` for that of the user `name`.
fn expand_user(path: &[u8]) -> Res<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let plain = |bytes: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    let Some(named) = path.strip_prefix(b"~") else {
        return Ok(plain(path));
    };
    let (user, rest) = match named.iter().position(|b| *b == b'/') {
        Some(at) => (&named[..at], &named[at + 1..]),
        None => (named, &b""[..]),
    };
    let home = match user.is_empty() {
        true => std::env::var_os("HOME").map(PathBuf::from),
        false => alt_os::users::home_of(&std::ffi::CString::new(user)?)?,
    };
    let unknown = || {
        let path = String::from_utf8_lossy(path);
        fatal(format!("failed to expand user dir in: '{path}'"))
    };
    Ok(home.ok_or_else(unknown)?.join(plain(rest)))
}
