//! What the refs lead to and the store does not hold.
//!
//! Every record can read while a commit names an object no record holds:
//! one whose records were taken out on request, or one that never came
//! with a copy of the store. The check walks from every ref through tags,
//! commits and trees and names each such object once, with the first
//! commit and path found that name it.

use std::collections::HashSet;

use super::{Res, with_cause};
use crate::native::Store;

/// A line for each ref that points at an object which does not read and,
/// unless `refs_only`, for each object below the refs that is not stored;
/// the refs are those before the last `dropped` operations of the log.
pub(super) fn missing(store: &Store, dropped: usize, refs_only: bool) -> Res<Vec<String>> {
    let dangling = store.dangling_refs(dropped)?;
    let mut lines: Vec<String> = dangling
        .iter()
        .map(|(name, oid)| format!("ref {name} points at {oid}, which is not stored"))
        .collect();
    if refs_only {
        return Ok(lines);
    }
    // a record a writer is about to cut off holds nothing for good
    let gone = store.odb().unconfirmed_objects().map_err(with_cause)?;
    let odb = store.odb();
    // an object the walk reads tells by itself whether it is stored
    let absent = |oid: &_, read: bool| Ok(gone.contains(oid) || !read && !odb.contains(oid)?);
    let taken = store.kept_lost()?.unwrap_or_default();
    let told: HashSet<_> = dangling.iter().map(|(_, oid)| *oid).collect();
    for named in store.reaching(dropped, true, &absent)? {
        if named.tip && told.contains(&named.oid) {
            continue;
        }
        let why = match taken.contains(&named.oid) {
            true => {
                "; its records were taken out with `alt fsck --drop-lost --yes --committed`: \
                 add the file again, or fetch the object from another copy of the repository"
            }
            false => "",
        };
        let (kind, oid, by) = (named.kind, named.oid, named.by);
        lines.push(format!("missing {kind} {oid}: {by}{why}"));
    }
    Ok(lines)
}
