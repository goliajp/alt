//! The repository's git-style configuration, as kept with the store.

use super::*;

impl NativeRepo<'_> {
    /// The configuration that applies in this repository, includes
    /// resolved: the system's, the user's, and on top the repository's
    /// own file.
    pub(super) fn settings(&self) -> Res<alt_git_config::Config> {
        let dir = self.store.alt_dir.join("git-import");
        let file = dir.join("config");
        let branch = self.head_branch().ok().and_then(|full| {
            let short = full.strip_prefix("refs/heads/")?;
            Some(BString::from(short.as_bytes().to_vec()))
        });
        let mut entries = crate::git_settings::outer(Some(dir.clone()), branch.clone())?;
        if file.is_file() {
            let ctx = alt_git_config::IncludeContext {
                git_dir: Some(dir),
                branch,
                home: std::env::var_os("HOME").map(PathBuf::from),
            };
            entries.extend(alt_git_config::Config::load(&file, &ctx)?.entries);
        }
        Ok(alt_git_config::Config { entries })
    }
}
