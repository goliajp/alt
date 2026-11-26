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
/// its trees or as the bases of its deltas.
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
/// names as a parent or a base, or as the new target of a ref. Where a ref it
/// does not update is gone by now, or points where its old target is not an
/// ancestor, what it led to may be led to by nothing any more, and the push
/// would have it back by its id. A push is refused for it, and reads the
/// refs again, where it leans on such an object:
///
/// - a commit it names (`leans`, and the new targets of its updates) that
///   such a ref led to, and that no ref it was read with leads to now;
/// - another object (a tree, a blob), unless it is in a tree of one of those
///   commits that a ref leads to now. That is where a Git client takes the
///   objects a thin pack leans on from; an object it leans on beyond them is
///   refused while a ref moved, though it may be led to still, as finding
///   that out takes a walk of the history of the refs.
///
/// A push that leaned on more than was kept of what it leans on (see
/// [`Leans::whole`]) is refused wherever a ref moved. A ref that moved and led
/// to none of it changes nothing for the push.
/// `current` is read with the row of the repository held.
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
    let now: Vec<Oid> = (read.iter())
        .filter_map(|(name, _)| current.refs.get(name).map(|now| raw(*now)))
        .collect();
    let named = (publication.refs.iter())
        .filter_map(|update| update.new.as_deref())
        .filter_map(|new| alt_git_codec::ObjectId::from_hex(new.as_bytes()).ok())
        .map(raw);
    let mut commits: Vec<Oid> = leans.commits.iter().copied().chain(named).collect();
    commits.retain(|commit| set.commit(commit).is_some());
    let floor = commits.iter().filter_map(|commit| set.commit(commit));
    let Some(floor) = floor.map(|commit| commit.generation).min() else {
        return match leans.others.is_empty() {
            true => Ok(()),
            false => Err(Error::Precondition(REFS_MOVED)),
        };
    };
    let led_before = reached(set, &moved, floor, &mut remaining)?;
    let led_now = reached(set, &now, floor, &mut remaining)?;
    if commits
        .iter()
        .any(|commit| led_before.contains(commit) && !led_now.contains(commit))
    {
        return Err(Error::Precondition(REFS_MOVED));
    }
    if leans.others.is_empty() {
        return Ok(());
    }
    let trees = (commits.iter())
        .filter(|commit| led_now.contains(*commit))
        .filter_map(|commit| set.commit(commit).map(|commit| commit.tree));
    match in_trees(set, trees.collect(), &leans.others, &mut remaining)? {
        true => Ok(()),
        false => Err(Error::Precondition(REFS_MOVED)),
    }
}

/// Whether every object of `others` is in the trees `trees` or below them.
fn in_trees(
    set: &SegmentSet,
    mut pending: Vec<Oid>,
    others: &[Oid],
    remaining: &mut usize,
) -> Result<bool> {
    let mut others: alt_hash::KeyedIdSet<Oid> = others.iter().copied().collect();
    let mut seen = alt_hash::KeyedIdSet::default();
    while let Some(tree) = pending.pop() {
        if others.is_empty() {
            break;
        }
        if !seen.insert(tree) {
            continue;
        }
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("Git tree walk"))?;
        let (_, data) = (set.read(&tree)).map_err(|_| Error::Integrity("pack segment object"))?;
        for entry in alt_git_codec::Tree::raw_entries(&data, alt_git_codec::HashAlgo::Sha1) {
            let (mode, _, oid) = entry.map_err(|_| Error::Integrity("pack segment tree"))?;
            let oid: Oid = oid.try_into().expect("sha1 object id");
            others.remove(&oid);
            if mode.value() & 0o170000 == 0o040000 {
                pending.push(oid);
            }
        }
    }
    Ok(others.is_empty())
}

/// The commits `tips` lead to down to generation `floor`, the tips among them.
fn reached(
    set: &SegmentSet,
    tips: &[Oid],
    floor: u32,
    remaining: &mut usize,
) -> Result<alt_hash::KeyedIdSet<Oid>> {
    let mut seen = alt_hash::KeyedIdSet::default();
    let mut pending = tips.to_vec();
    while let Some(oid) = pending.pop() {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("Git fast-forward walk"))?;
        let Some(commit) = set.commit(&oid) else {
            continue;
        };
        if commit.generation < floor || !seen.insert(oid) {
            continue;
        }
        pending.extend(commit.parents);
    }
    Ok(seen)
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
