//! The repository's git-style configuration, as kept with the store.

use super::*;

/// The files outside the working tree that a scan of it goes by.
pub(super) struct ScanInputs {
    /// those that decide how a file compares, there or not; which they
    /// are also depends on the branch, since an `includeIf` of the
    /// configuration may ask for it
    pub(super) files: Vec<PathBuf>,
    /// the ignore files, a later one above an earlier
    pub(super) excludes: Vec<PathBuf>,
}

impl NativeRepo<'_> {
    pub(super) fn scan_inputs(&self) -> Res<ScanInputs> {
        let (settings, mut files) = self.settings_listed()?;
        let (global, info) = self.attribute_files(&settings)?;
        files.extend(global);
        files.push(info);
        Ok(ScanInputs {
            files,
            excludes: self.exclude_files(&settings)?,
        })
    }

    /// The ignore files outside the working tree, the one whose rules
    /// yield to the other's first: the user's, which `core.excludesFile`
    /// names, and the repository's own. An empty setting names no file,
    /// and a relative one a file in the working tree, as for git.
    pub(super) fn exclude_files(&self, settings: &alt_git_config::Config) -> Res<Vec<PathBuf>> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let user = match settings.get_str("core", None, "excludesfile") {
            Some(path) if path.is_empty() => None,
            Some(path) => Some(self.root.join(convert::expand_home(path, home.as_deref())?)),
            None => alt_worktree::default_excludes_file(),
        };
        let info = self.store.alt_dir.join("git-import/info/exclude");
        Ok(user.into_iter().chain([info]).collect())
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
