//! What an import keeps of a git repository beside its objects and refs.

use std::fs;
use std::path::Path;

use alt_repo::Repository;

use crate::ImportError;

/// Keeps the configuration of `repo` as it is written, its attributes,
/// which decide how files are checked out, and the logs of its refs.
/// `last` is the last op of the store once the refs are brought.
/// `ours` are the logs the store had of its refs before.
pub(crate) fn keep(
    repo: &Repository,
    alt_dir: &Path,
    last: Option<String>,
    ours: alt_repo::reflog::Carried,
) -> Result<(), ImportError> {
    alt_repo::kept_config::take(repo.git_dir(), alt_dir)?;
    let attributes = repo.git_dir().join("info/attributes");
    if attributes.is_file() {
        let to = alt_dir.join("git-import/info");
        fs::create_dir_all(&to)?;
        fs::copy(&attributes, to.join("attributes"))?;
    }
    let ours = (ours, repo.algo()?);
    Ok(alt_repo::reflog::take(repo.git_dir(), alt_dir, last, ours)?)
}
