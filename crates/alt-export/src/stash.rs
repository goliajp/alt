//! The stack of stashed entries, written as git keeps it: the log of
//! `refs/stash`.

use std::path::Path;

use alt_git_codec::{Commit, HashAlgo, ObjectId};
pub use alt_git_refs::stash::{ENTRIES, TOP, entry_number};
use alt_git_refs::stash::{Logged, write_log as write};
use alt_odb::NativeOdb;

use crate::ExportError;

/// Writes the log of `refs/stash` for `entries`, each with its number;
/// nothing where there are none.
pub(crate) fn write_log(
    odb: &NativeOdb,
    git_dir: &Path,
    mut entries: Vec<(u64, ObjectId)>,
    algo: HashAlgo,
) -> Result<(), ExportError> {
    entries.sort();
    let mut commits = Vec::new();
    for (_, commit) in &entries {
        let object = odb.get(commit)?;
        let object = object.ok_or(ExportError::Format("a stashed commit is not stored"))?;
        let parsed = Commit::parse(&object.data)
            .map_err(|_| ExportError::Format("a stashed entry is no commit"))?;
        commits.push((*commit, parsed));
    }
    let mut logged = Vec::new();
    for (commit, parsed) in &commits {
        let by = parsed.committer();
        let by = by.ok_or(ExportError::Format("a stashed commit names no committer"))?;
        let subject = parsed.message().split(|b| *b == b'\n').next();
        logged.push(Logged {
            commit: *commit,
            by,
            subject: subject.unwrap_or_default(),
        });
    }
    Ok(write(git_dir, &logged, algo)?)
}
