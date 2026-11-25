//! Whether what the refs a push was read with led to is still led to when it is
//! published.

use crate::{Error, Refs, Result};
use alt_pack_segment::{Oid, SegmentSet};
use alt_remote_protocol::Publication;
use alt_repository_service::RepositorySnapshot;
use std::sync::{Arc, Mutex};

/// what a push is refused with where a ref it was read with moved off what it
/// led to before the push was published
pub const REFS_MOVED: &str = "a ref the push was read with moved before it was published";

/// Called with the directory of the stored bytes of a push that was verified,
/// before the row of its repository is held to publish it: for a test to move
/// a ref meanwhile, as another push may.
#[doc(hidden)]
pub static PUSH_VERIFIED: Mutex<Option<PushVerified>> = Mutex::new(None);
pub type PushVerified = Arc<dyn Fn(&std::path::Path) + Send + Sync>;

pub(super) fn push_verified(root: &std::path::Path) {
    let verified = PUSH_VERIFIED.lock().expect("push verified").clone();
    if let Some(verified) = verified {
        verified(root);
    }
}

/// A push may lean on any object the refs it was read with led to: one it
/// names as a parent or a base, or as the new target of a ref. Where one of
/// those refs that the push does not update is gone by now, or points where
/// its old target is not an ancestor, that object may be led to by nothing
/// any more, and the push would have it back by its id: it is refused, and
/// read the refs again. `current` is read with the row of the repository held.
pub(super) fn still_led(
    current: &RepositorySnapshot,
    publication: &Publication,
    read: &Refs,
    set: &SegmentSet,
    steps: usize,
) -> Result<()> {
    let mut remaining = steps;
    for (name, old) in read.iter() {
        if publication.refs.iter().any(|update| update.name == name) {
            continue;
        }
        let kept = match current.refs.get(name) {
            Some(now) if *now == old => true,
            Some(now) => ancestor(set, &raw(old), &raw(*now), &mut remaining)?,
            None => false,
        };
        if !kept {
            return Err(Error::Precondition(REFS_MOVED));
        }
    }
    Ok(())
}

fn raw(oid: alt_git_codec::ObjectId) -> Oid {
    oid.as_bytes().try_into().expect("sha1 object id")
}

/// Whether `old` is an ancestor of `new`; generations prune everything older
/// than `old`.
pub fn ancestor(set: &SegmentSet, old: &Oid, new: &Oid, remaining: &mut usize) -> Result<bool> {
    let Some(floor) = set.commit(old).map(|commit| commit.generation) else {
        return Ok(false);
    };
    let mut seen = alt_hash::KeyedIdSet::default();
    let mut pending = vec![*new];
    while let Some(oid) = pending.pop() {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("Git fast-forward walk"))?;
        if oid == *old {
            return Ok(true);
        }
        if !seen.insert(oid) {
            continue;
        }
        if let Some(commit) = set.commit(&oid)
            && commit.generation > floor
        {
            pending.extend(commit.parents);
        }
    }
    Ok(false)
}
