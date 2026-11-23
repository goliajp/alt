//! The stack of stashed entries, written as git keeps it: the log of
//! `refs/stash`.

use std::collections::HashSet;
use std::path::Path;

use alt_git_codec::{Commit, HashAlgo, ObjectId};
pub use alt_git_refs::stash::{ENTRIES, TOP, entry_number};
use alt_git_refs::stash::{Logged, write_log as write};
use alt_odb::NativeOdb;
use alt_refs::{RefStore, RefTarget};

use crate::ExportError;

/// Writes the log of `refs/stash` for the entries among `refs`, but
/// those that name one of the `unstored`; nothing where there are none.
pub(crate) fn write_log(
    odb: &NativeOdb,
    git_dir: &Path,
    refs: &RefStore,
    unstored: &HashSet<ObjectId>,
    algo: HashAlgo,
) -> Result<(), ExportError> {
    let entry = |(name, target): (&str, &RefTarget)| match target {
        RefTarget::Oid(oid) if !unstored.contains(oid) => Some((entry_number(name)?, *oid)),
        _ => None,
    };
    let mut entries: Vec<(u64, ObjectId)> = refs.iter().filter_map(entry).collect();
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
