//! What an export leaves the git repository with, told in a file of
//! that repository for the next import from it.

use std::collections::HashSet;
use std::path::Path;

use alt_bytes::BString;
use alt_git_codec::ObjectId;
use alt_refs::{RefStore, RefTarget};

use crate::{ExportError, stash};

/// Leaves in the repository at `git_dir` what the export leaves it
/// with: an import from it tells by that what git changed since. The
/// store itself is not written: an export only reads it.
pub(crate) fn keep(
    alt_dir: &Path,
    git_dir: &Path,
    refs: &RefStore,
    head: Option<&alt_git_refs::RefTarget>,
    loose: &[(BString, alt_git_refs::RefTarget)],
    unstored: &HashSet<ObjectId>,
) -> Result<(), ExportError> {
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
    Ok(alt_repo::met::leave(alt_dir, git_dir, left)?)
}
