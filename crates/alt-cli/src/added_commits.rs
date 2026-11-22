//! What a push adds to the history a repository serves: the objects a
//! new value of the push reaches and no ref reached before it. All of
//! them have to be held, by the object store or by the push's pack, and
//! the commits among them have to pass the policy of the pusher.
//!
//! This is the check Git's `receive-pack` makes before it moves a ref
//! (`rev-list --objects <new values> --not --all`), and like there what
//! the refs reach already is taken as whole: it was looked at when it
//! came in. An object missing under the refs is passed over, and a push
//! that brings it along does not have it written.
//!
//! Two marks are passed down from commit to parent, one from the new
//! values and one from the refs as they are. Commits are visited newest
//! committer date first, so that the mark of the refs usually gets to a
//! commit before the commit is visited. The dates decide that order and
//! nothing else: a commit that gets a mark after it was visited is
//! visited again and passes the mark on.
//!
//! A commit visited with the mark of the push alone is asked whether it
//! fails: by the policy, or because a parent of it is not held. One that
//! does stays a suspect until the mark of the refs reaches it. Every
//! commit the push adds is visited that way: the commits between it and
//! a new value are added too, so none of them ever has the other mark.
//! The walk ends
//!
//! - when no commit with the mark of the push alone waits to be visited
//!   and no suspect is left: no commit the push adds fails, or
//! - when nothing waits to be visited: every commit under the refs has
//!   their mark by then, so a suspect left is a commit the push adds.
//!
//! The trees of the commits found added are looked at then (see
//! [`trees`]); a commit whose tree is not whole is a suspect like any
//! other, and the walk goes on for it.
//!
//! A commit is read when it gets its first mark and queued once for each
//! mark, which bounds the walk by the commits reachable, whatever their
//! dates. The objects the refs point at are not read before the walk
//! comes to a commit that was held before the push and that no ref
//! points at: a push on top of what a ref points at reads none of them.

mod trees;
mod walk;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tree_tests;

use std::collections::HashSet;

use alt_git_codec::{ObjectId, ObjectKind, RawObject, Tag};
use walk::{ADDED, Walk};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// What a push is refused with when it adds an object that is not held.
pub const MISSING: &str = "missing necessary objects";

/// Where the walk reads from.
pub struct Source<'a> {
    /// an object, `None` when it is not held
    pub get: &'a dyn Fn(&ObjectId) -> Res<Option<RawObject>>,
    /// whether an object is held
    pub holds: &'a dyn Fn(&ObjectId) -> Res<bool>,
    /// whether an object was held before the push
    pub had: &'a dyn Fn(&ObjectId) -> Res<bool>,
    /// why a commit fails the policy, when it does
    pub fails: &'a dyn Fn(&ObjectId) -> Res<Option<String>>,
}

/// What a tag names, `Err` with the reason for a tag that does not read
/// as one; `None` for anything else.
fn tagged(source: &Source<'_>, oid: &ObjectId) -> Res<Option<Result<ObjectId, String>>> {
    Ok(match (source.get)(oid)? {
        Some(raw) if raw.kind == ObjectKind::Tag => {
            let target = Tag::parse(&raw.data).map_err(|e| e.to_string());
            let target = target.and_then(|tag| tag.object().ok_or("no object".to_owned()));
            Some(target.map_err(|e| format!("tag {oid} unreadable: {e}")))
        }
        _ => None,
    })
}

/// The objects a push adds that points refs at `new`, where the refs
/// point at `served`; or the reason to refuse it: an object it adds is
/// not held or does not read, or a commit it adds fails the policy. An
/// object under the refs that is not held or does not read is passed
/// over: it was so before the push.
pub fn added(
    source: &Source<'_>,
    served: &[ObjectId],
    new: &[ObjectId],
) -> Res<Result<HashSet<ObjectId>, String>> {
    let mut walk = Walk::new(source, served);
    let mut objects = HashSet::new();
    for &tip in new {
        let mut oid = tip;
        while !walk.tip(&oid) && !walk.mark(oid, ADDED)? {
            let Some(raw) = (source.get)(&oid)? else {
                return Ok(Err(MISSING.to_owned()));
            };
            objects.insert(oid);
            match raw.kind {
                ObjectKind::Tag => match tagged(source, &oid)? {
                    Some(Ok(target)) => oid = target,
                    Some(Err(reason)) => return Ok(Err(reason)),
                    None => break,
                },
                ObjectKind::Tree if !walk.trees.whole(source, oid, &[])? => {
                    return Ok(Err(MISSING.to_owned()));
                }
                _ => break,
            }
        }
    }
    if let Some(reason) = walk.refusal()? {
        return Ok(Err(reason));
    }
    objects.extend(walk.added_objects());
    Ok(Ok(objects))
}
