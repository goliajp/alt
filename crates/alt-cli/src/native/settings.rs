//! The repository's git-style configuration, as kept with the store.

use super::*;

impl NativeRepo<'_> {
    /// The files outside the working tree that decide how a scan compares
    /// a file, there or not, and what else does: the branch, which an
    /// `includeIf` of the configuration may ask for.
    pub(super) fn scan_inputs(&self) -> Res<(Vec<PathBuf>, Vec<u8>)> {
        let (settings, mut files) = self.settings_listed()?;
        let (global, info) = self.attribute_files(&settings)?;
        files.extend(global);
        files.push(info);
        Ok((files, self.head_branch()?.into_bytes()))
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
