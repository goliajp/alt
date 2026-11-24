//! What an export leaves the git repository with, kept for the next
//! import from it.

use std::collections::HashSet;
use std::path::Path;

use alt_bytes::BString;
use alt_git_codec::ObjectId;
use alt_refs::{RefStore, RefTarget};

use crate::{ExportError, stash};

/// Keeps what the export leaves the repository at `git_dir` with as
/// what it has: an import from it tells by that what git changed since.
/// An export that reads past damage writes nothing into the store.
pub(crate) fn keep(
    alt_dir: &Path,
    git_dir: &Path,
    refs: &RefStore,
    head: Option<&alt_git_refs::RefTarget>,
    loose: &[(BString, alt_git_refs::RefTarget)],
    unstored: &HashSet<ObjectId>,
) -> Result<(), ExportError> {
    if alt_odb::reading_past_damage() {
        return Ok(());
    }
    let ours = |target: &alt_git_refs::RefTarget| match target {
        alt_git_refs::RefTarget::Direct(oid) => Some(RefTarget::Oid(*oid)),
        alt_git_refs::RefTarget::Symbolic(name) => Some(RefTarget::Symbolic(
            std::str::from_utf8(name).ok()?.to_owned(),
        )),
    };
    let mut left: Vec<(String, RefTarget)> = Vec::new();
    left.extend(head.and_then(ours).map(|head| ("HEAD".to_owned(), head)));
    for (name, target) in loose {
        if let (Ok(name), Some(target)) = (std::str::from_utf8(name), ours(target)) {
            left.push((name.to_owned(), target));
        }
    }
    let entry = |(name, target): (&str, &RefTarget)| match target {
        RefTarget::Oid(oid) if stash::entry_number(name).is_some() && !unstored.contains(oid) => {
            Some((name.to_owned(), target.clone()))
        }
        _ => None,
    };
    left.extend(refs.iter().filter_map(entry));
    let left = left.iter().map(|(name, target)| (name.as_str(), target));
    Ok(alt_repo::met::write(alt_dir, git_dir, left)?)
}
