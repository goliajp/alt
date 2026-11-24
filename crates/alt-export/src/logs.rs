//! The logs of the refs, written as git keeps them: a file for each ref
//! under `logs/`.

use std::fs;
use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_git_refs::reflog::write;
use alt_repo::Repository;

use crate::ExportError;

/// Writes the log of each of `names` that has one under `git_dir`. git
/// logs its head, its branches and its remote-tracking branches.
pub(crate) fn write_logs<'a>(
    alt_dir: &Path,
    git_dir: &Path,
    names: impl Iterator<Item = &'a str>,
    also: Option<&alt_refs::AlsoNamed>,
    algo: HashAlgo,
) -> Result<(), ExportError> {
    let mut repo = Repository::open_alt(alt_dir.to_owned(), None)?.reading();
    if let Some(also) = also {
        repo = repo.naming(also.clone());
    }
    let logged = |name: &&str| {
        *name == "HEAD" || name.starts_with("refs/heads/") || name.starts_with("refs/remotes/")
    };
    for name in names.filter(logged) {
        let lines = match repo.log_lines(name) {
            // read past damage, a log that tells of a commit that is not
            // stored is left out, as the ref of such a commit is
            Err(alt_repo::RepoError::MissingObject(_)) if alt_odb::reading_past_damage() => {
                continue;
            }
            lines => lines?,
        };
        if lines.is_empty() {
            continue;
        }
        let path = git_dir.join("logs").join(name);
        fs::create_dir_all(path.parent().ok_or(ExportError::Format("bad log path"))?)?;
        fs::write(path, write(&lines, algo))?;
    }
    Ok(())
}
