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
        let user = match self.named_file(settings, "excludesfile")? {
            Some(named) => named,
            None => alt_worktree::default_excludes_file(),
        };
        if let Some(named) = user.as_ref().filter(|named| named.is_dir()) {
            let named = named.display();
            return Err(fatal(format!("cannot use {named} as an exclude file")));
        }
        let info = self.store.alt_dir.join("git-import/info/exclude");
        Ok(user.into_iter().chain([info]).collect())
    }

    /// The attribute files outside the working tree: the user's, which
    /// `core.attributesFile` names, and the repository's own.
    pub(super) fn attribute_files(
        &self,
        settings: &alt_git_config::Config,
    ) -> Res<(Option<PathBuf>, PathBuf)> {
        let user = match self.named_file(settings, "attributesfile")? {
            Some(named) => named,
            None => std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .map(|config| config.join("git").join("attributes")),
        };
        let info = self.store.alt_dir.join("git-import/info/attributes");
        Ok((user, info))
    }

    /// The file the setting `key` of the `core` section names, as git
    /// reads it: `None` when it is not set, no file when it is set to
    /// nothing, a relative path as one in the working tree. A key without
    /// a value is an error, and so is a home directory that is not known.
    fn named_file(
        &self,
        settings: &alt_git_config::Config,
        key: &str,
    ) -> Res<Option<Option<PathBuf>>> {
        match settings.get("core", None, key) {
            None => Ok(None),
            Some(None) => {
                let missing = format!("error: missing value for 'core.{key}'\n");
                crate::diagnostics::emit(missing.as_bytes())?;
                Err(fatal(format!("bad config variable 'core.{key}'")))
            }
            Some(Some(path)) if path.is_empty() => Ok(Some(None)),
            Some(Some(path)) => Ok(Some(Some(self.root.join(expand_user(path)?)))),
        }
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
