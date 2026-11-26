//! Whether what the refs a push was read with led to is still led to when it is
//! published.

use crate::{Error, Refs, Result};
use alt_git_codec::ObjectId;
use alt_pack_segment::{Oid, SegmentSet};
use alt_remote_protocol::Publication;
use alt_repository_service::RepositorySnapshot;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

/// what a push is refused with where a ref it was read with moved off what it
/// led to before the push was published
pub const REFS_MOVED: &str = "a ref the push was read with moved before it was published";

/// what a push is refused with where a ref it was read with moved, and finding
/// out whether the push leans on what it led to takes more than [`LOST_WALK`]
pub const LOST_UNTOLD: &str = "a ref the push was read with moved, and what it led to is more than is looked through for a push";

/// The steps the walk that finds what a push may have lost takes at the most:
/// the commits it goes through and the trees it reads. It is taken before the
/// row of the repository is held, and apart from the walks of requests: a
/// tag deleted far down a long history is not looked for beyond this, and the
/// push is refused ([`LOST_UNTOLD`]); sent again, it is read with the refs as
/// they are then. Some 100,000 steps take a few seconds.
#[doc(hidden)]
pub static LOST_WALK: AtomicUsize = AtomicUsize::new(100_000);

/// Called with the directory of the stored bytes of a push once what it may
/// have lost was found and before the row of its repository is held: for a
/// test to move a ref meanwhile.
#[doc(hidden)]
pub static PUSH_FOUND: Mutex<Option<PushVerified>> = Mutex::new(None);

pub(super) fn push_found(root: &std::path::Path) {
    let found = PUSH_FOUND.lock().expect("push found").clone();
    if let Some(found) = found {
        found(root);
    }
}

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
/// to none of it changes nothing for the push.
///
/// This is found with the refs `refs` as they are before the row of the
/// repository is held, within [`LOST_WALK`] steps, in `set`, which holds what
/// they lead to. What is found holds where the refs then still are where they
/// were, or have moved on from there ([`Led::holds`]).
pub(super) fn found(
    refs: &BTreeMap<String, ObjectId>,
    updated: &[Updated<'_>],
    (read, leans): (&Refs, &Leans),
    set: &SegmentSet,
) -> Result<Led> {
    let then = (read.iter())
        .filter(|(name, _)| !updated.iter().any(|(updated, _)| updated == name))
        .map(|(name, _)| (name.to_owned(), refs.get(name).copied()));
    let led = Led {
        then: then.collect(),
    };
    let mut remaining = LOST_WALK.load(Relaxed);
    match lost_leant(refs, updated, (read, leans), set, &mut remaining) {
        Ok(()) => Ok(led),
        Err(Error::Limit(_)) => Err(Error::Precondition(LOST_UNTOLD)),
        Err(error) => Err(error),
    }
}

/// A ref a push updates, and what it names after, where anything.
pub(super) type Updated<'a> = (&'a str, Option<ObjectId>);

/// The refs of `publication` and what each names after.
pub(super) fn updated(publication: &Publication) -> Vec<Updated<'_>> {
    let new = |new: &Option<String>| new.as_deref().and_then(|new| new.parse().ok());
    let refs = publication.refs.iter();
    refs.map(|update| (update.name.as_str(), new(&update.new)))
        .collect()
}

/// The refs a push was read with that it does not update, each with where it
/// was once what the push may have lost was found.
pub(super) struct Led {
    then: Vec<(String, Option<ObjectId>)>,
}

impl Led {
    /// Whether what was found holds with the refs as they are now, `current`,
    /// read with the row of the repository held: where each ref is where it
    /// was then, was gone then, or moved on from there. A ref that moved on
    /// leads to more than it did, and nothing that was found not lost is lost
    /// by it; one that was deleted or moved elsewhere since may lose what
    /// was not looked for, and the push is refused ([`REFS_MOVED`]).
    pub(super) fn holds(
        &self,
        current: &RepositorySnapshot,
        set: &SegmentSet,
        steps: usize,
    ) -> Result<()> {
        let mut remaining = steps;
        for (name, then) in &self.then {
            let on = match (then, current.refs.get(name)) {
                (None, _) => true,
                (Some(then), Some(now)) if then == now => true,
                (Some(then), Some(now)) => ancestor(set, &raw(*then), &raw(*now), &mut remaining)?,
                (Some(_), None) => false,
            };
            if !on {
                return Err(Error::Precondition(REFS_MOVED));
            }
        }
        Ok(())
    }
}

/// Fails where the push leans on, or names, what is lost (see [`found`]).
fn lost_leant(
    refs: &BTreeMap<String, ObjectId>,
    updated: &[Updated<'_>],
    (read, leans): (&Refs, &Leans),
    set: &SegmentSet,
    remaining: &mut usize,
) -> Result<()> {
    let mut moved = Vec::new();
    for (name, old) in read.iter() {
        if updated.iter().any(|(updated, _)| *updated == name) {
            continue;
        }
        let kept = match refs.get(name) {
            Some(now) if *now == old => true,
            Some(now) => ancestor(set, &raw(old), &raw(*now), remaining)?,
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
    let (now, named_now) = now(refs, (read, updated), set, remaining)?;
    let named = updated.iter().filter_map(|(_, new)| *new).map(raw);
    let lost = match set.select_while(&moved, &now, *remaining, None) {
        Ok(lost) => lost,
        Err(alt_pack_segment::Error::Limit(what)) => return Err(Error::Limit(what)),
        Err(_) => return Err(Error::Integrity("pack segment object")),
    };
    let lost: alt_hash::KeyedIdSet<&Oid> = (lost.objects().iter())
        .filter(|oid| !named_now.contains(*oid))
        .collect();
    let mut leant = (leans.commits.iter().chain(&leans.others).copied()).chain(named);
    match leant.any(|oid| lost.contains(&oid)) {
        true => Err(Error::Precondition(REFS_MOVED)),
        false => Ok(()),
    }
}

/// What the refs `read` lead to now: the commits they name, through their
/// annotated tags, and what is named on the way to them, which is not lost.
/// A ref the push updates is taken where it was read: the push is published
/// only where it is still there, wherever it was meanwhile.
fn now(
    refs: &BTreeMap<String, ObjectId>,
    (read, updated): (&Refs, &[Updated<'_>]),
    set: &SegmentSet,
    remaining: &mut usize,
) -> Result<(Vec<Oid>, alt_hash::KeyedIdSet<Oid>)> {
    let (mut now, mut named) = (Vec::new(), alt_hash::KeyedIdSet::default());
    for (name, old) in read.iter() {
        let target = match updated.iter().any(|(updated, _)| *updated == name) {
            true => Some(old),
            false => refs.get(name).copied(),
        };
        if let Some(target) = target {
            now.push(peeled(set, raw(target), &mut named, remaining)?);
        }
    }
    Ok((now, named))
}

/// What the annotated tags of `oid` name, through each of them, where they are
/// stored: `oid` where it is no tag. What is passed on the way, and what is
/// named where it is no commit, is put in `named`.
fn peeled(
    set: &SegmentSet,
    mut oid: Oid,
    named: &mut alt_hash::KeyedIdSet<Oid>,
    remaining: &mut usize,
) -> Result<Oid> {
    let bad = |_| Error::Integrity("pack segment object");
    loop {
        match set.kind(&oid).map_err(bad)? {
            Some(alt_pack_segment::Kind::Tag) => {}
            Some(alt_pack_segment::Kind::Commit) => return Ok(oid),
            _ => {
                named.insert(oid);
                return Ok(oid);
            }
        }
        named.insert(oid);
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("Git tag peel walk"))?;
        let (_, data) = set.read(&oid).map_err(bad)?;
        let target =
            alt_git_codec::Tag::parse(&data).map_err(|_| Error::Integrity("tag object"))?;
        oid = raw(target.object().ok_or(Error::Integrity("tag object"))?);
    }
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
