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

/// What of the segments a push leans on, each by its id: the commits it
/// names, as parents or otherwise, and the other objects, which it names in
/// its trees or as the bases of its deltas; and whether that is all of it.
pub struct Leans {
    pub commits: Vec<Oid>,
    pub others: Vec<Oid>,
    /// the pack leaned on no more than was kept of what it leans on
    pub whole: bool,
}

impl Leans {
    /// what a pack verified as `leans` has it lean on, where that is `whole`
    pub fn of(leans: &[(Oid, alt_pack_segment::Kind)], whole: bool) -> Self {
        let (commits, others): (Vec<_>, Vec<_>) =
            (leans.iter()).partition(|(_, kind)| *kind == alt_pack_segment::Kind::Commit);
        let ids = |leans: Vec<&(Oid, _)>| leans.into_iter().map(|(oid, _)| *oid).collect();
        Leans {
            commits: ids(commits),
            others: ids(others),
            whole,
        }
    }
}

/// A push may lean on the objects the refs it was read with led to: one it
/// names as a parent or a base, in a tree, or as the new target of a ref.
/// Where a ref it does not update is gone by now, or points where its old
/// target is not an ancestor, what it led to may be led to by nothing any
/// more, and the push would have it back by its id. A push is refused for it,
/// and reads the refs again, where an object it leans on, or names as a new
/// target, whatever its kind, is lost: led to by such a ref, through its
/// annotated tags too, and by none of the refs it was read with as they are
/// now. What is lost is found as a fetch finds what a client lacks: the
/// objects the old targets lead to that are not in the history the refs lead
/// to now, a tree and what is under it compared at the same path. An object
/// that the history now has at another path only is taken as lost: the push
/// is refused though it need not be, and is taken when it is sent again.
///
/// A push that leaned on more than was kept of what it leans on (see
/// [`Leans::whole`]) is refused wherever a ref moved. A ref that moved and led
/// to none of it changes nothing for the push. `current` is read with the row
/// of the repository held.
pub(super) fn still_led(
    current: &RepositorySnapshot,
    publication: &Publication,
    (read, leans): (&Refs, &Leans),
    set: &SegmentSet,
    steps: usize,
) -> Result<()> {
    let mut remaining = steps;
    let mut moved = Vec::new();
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
            moved.push(raw(old));
        }
    }
    if moved.is_empty() {
        return Ok(());
    }
    if !leans.whole {
        return Err(Error::Precondition(REFS_MOVED));
    }
    let mut now = Vec::new();
    for (name, _) in read.iter() {
        if let Some(target) = current.refs.get(name) {
            now.push(peeled(set, raw(*target), &mut remaining)?);
        }
    }
    let named = (publication.refs.iter())
        .filter_map(|update| update.new.as_deref())
        .filter_map(|new| alt_git_codec::ObjectId::from_hex(new.as_bytes()).ok())
        .map(raw);
    let lost = match set.select_while(&moved, &now, remaining, None) {
        Ok(lost) => lost,
        // finding out is more than a push may take: it reads the refs again
        Err(alt_pack_segment::Error::Limit(_)) => return Err(Error::Precondition(REFS_MOVED)),
        Err(_) => return Err(Error::Integrity("pack segment object")),
    };
    let lost: alt_hash::KeyedIdSet<&Oid> = lost.objects().iter().collect();
    let mut leant = (leans.commits.iter().chain(&leans.others).copied()).chain(named);
    match leant.any(|oid| lost.contains(&oid)) {
        true => Err(Error::Precondition(REFS_MOVED)),
        false => Ok(()),
    }
}

/// What the annotated tags of `oid` name, through each of them, where they are
/// stored: `oid` where it is no tag.
fn peeled(set: &SegmentSet, mut oid: Oid, remaining: &mut usize) -> Result<Oid> {
    let bad = |_| Error::Integrity("pack segment object");
    while set.kind(&oid).map_err(bad)? == Some(alt_pack_segment::Kind::Tag) {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("Git tag peel walk"))?;
        let (_, data) = set.read(&oid).map_err(bad)?;
        let target =
            alt_git_codec::Tag::parse(&data).map_err(|_| Error::Integrity("tag object"))?;
        oid = raw(target.object().ok_or(Error::Integrity("tag object"))?);
    }
    Ok(oid)
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
