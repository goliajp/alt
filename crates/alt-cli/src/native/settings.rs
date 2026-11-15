//! The repository's git-style configuration, as kept with the store.

use super::*;

impl NativeRepo<'_> {
    /// The configuration file of this repository with its includes
    /// resolved; empty when the repository has none.
    pub(super) fn settings(&self) -> Res<alt_git_config::Config> {
        let dir = self.store.alt_dir.join("git-import");
        let file = dir.join("config");
        if !file.is_file() {
            return Ok(alt_git_config::Config::default());
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
        Ok(alt_git_config::Config::load(&file, &ctx)?)
    }
}
