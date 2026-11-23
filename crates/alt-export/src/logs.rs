//! The logs of the refs, written as git keeps them: a file for each ref
//! under `logs/`.

use std::fs;
use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_git_refs::reflog::{Line, write};
use alt_repo::Repository;
use alt_repo::reflog::Entry;

use crate::ExportError;

/// Who git would say moved a ref at `entry`: as git logged it where it
/// did. An operation of the store tells when and no more: the committer
/// of the commit the ref was moved to stands for who.
fn by(repo: &Repository, entry: &Entry) -> Result<Vec<u8>, ExportError> {
    if let Some(by) = &entry.by {
        return Ok(by.clone());
    }
    let seconds = entry.at_ms.unwrap_or_default() / 1000;
    let mut who = Vec::new();
    if let Some(new) = entry.new
        && let Some(committer) = repo.read_commit(&new)?.committer()
        && let Some(end) = committer.iter().rposition(|b| *b == b'>')
    {
        who.extend_from_slice(&committer[..=end]);
    }
    who.extend_from_slice(format!(" {seconds} +0000").as_bytes());
    Ok(who)
}

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
        let entries = match repo.reflog(name) {
            // read past damage, a log that tells of a commit that is not
            // stored is left out, as the ref of such a commit is
            Err(alt_repo::RepoError::MissingObject(_)) if alt_odb::reading_past_damage() => {
                continue;
            }
            entries => entries?,
        };
        let mut lines = Vec::new();
        for entry in entries {
            lines.push(Line {
                old: entry.old,
                new: entry.new,
                by: by(&repo, &entry)?,
                said: entry.said,
            });
        }
        if lines.is_empty() {
            continue;
        }
        let path = git_dir.join("logs").join(name);
        fs::create_dir_all(path.parent().ok_or(ExportError::Format("bad log path"))?)?;
        fs::write(path, write(&lines, algo))?;
    }
    Ok(())
}
