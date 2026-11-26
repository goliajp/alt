//! What of a fetch the refs of a repository are walked for: whether what it
//! asks for is under them, and which of what it says it has is.

use alt_git_codec::{Commit, ObjectId, ObjectKind, Tag, Tree};
use alt_hash::KeyedIdSet;
use alt_repo::Repository;
use alt_wire::fetch::FetchRequest;

use super::until::Until;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The first object `req` wants that no ref of `repo` reaches. An object
/// that is in the store and under no ref is one of a push that was
/// refused, or of a branch that was deleted: it is not served.
///
/// What a clone and a fetch ask for are the objects the refs point at,
/// and those are told without a walk. The objects a partial clone left
/// out are asked for one by one, and are looked for from the refs down.
pub(super) fn unreached(
    repo: &Repository,
    req: &FetchRequest,
    until: &Until,
) -> Res<Option<ObjectId>> {
    let tips: Vec<ObjectId> = repo.list_refs()?.into_iter().map(|r| r.1).collect();
    let mut seen: KeyedIdSet<ObjectId> = tips.iter().copied().collect();
    let mut sought: KeyedIdSet<ObjectId> = req.wants.iter().copied().collect();
    sought.retain(|want| !seen.contains(want));
    let algo = repo.algo()?;
    let mut ahead = tips;
    while let Some(oid) = ahead.pop() {
        if sought.is_empty() {
            break;
        }
        until.go_on()?;
        let Some(object) = repo.read_object(&oid)? else {
            continue;
        };
        let mut under = Vec::new();
        match object.kind {
            ObjectKind::Commit => {
                let commit = Commit::parse(&object.data)?;
                under.extend(commit.tree());
                under.extend(commit.parents());
            }
            ObjectKind::Tag => under.extend(Tag::parse(&object.data)?.object()),
            ObjectKind::Tree => {
                for entry in Tree::parse(&object.data, algo)?.entries {
                    match entry.mode.object_kind() {
                        ObjectKind::Tree => under.push(entry.oid),
                        // a file is not read to know that it is reached
                        ObjectKind::Blob => drop(sought.remove(&entry.oid)),
                        _ => {}
                    }
                }
            }
            ObjectKind::Blob => {}
        }
        for oid in under {
            if seen.insert(oid) {
                sought.remove(&oid);
                ahead.push(oid);
            }
        }
    }
    Ok(req.wants.iter().find(|want| sought.contains(want)).copied())
}

/// Those of the commits `req` says its sender has that a ref of `repo`
/// reaches. One that is in the store and under no ref is taken as one
/// the server does not know: left out of the pack as a commit both
/// sides have, it would tell its sender that it is there.
pub(super) fn reached(repo: &Repository, req: &FetchRequest, until: &Until) -> Res<Vec<ObjectId>> {
    let tips: Vec<ObjectId> = repo.list_refs()?.into_iter().map(|r| r.1).collect();
    let mut seen: KeyedIdSet<ObjectId> = tips.iter().copied().collect();
    let mut sought: KeyedIdSet<ObjectId> = req.haves.iter().copied().collect();
    let mut reached: Vec<ObjectId> = Vec::new();
    reached.extend(req.haves.iter().filter(|have| seen.contains(have)));
    sought.retain(|have| !seen.contains(have));
    // one the store lacks is found under no ref: it is not looked for
    let mut held = Vec::new();
    for have in sought {
        until.go_on()?;
        held.extend(repo.read_object(&have)?.map(|_| have));
    }
    let mut sought: KeyedIdSet<ObjectId> = held.into_iter().collect();
    let mut ahead = tips;
    while let Some(oid) = ahead.pop() {
        if sought.is_empty() {
            break;
        }
        until.go_on()?;
        let Some(object) = repo.read_object(&oid)? else {
            continue;
        };
        let under: Vec<ObjectId> = match object.kind {
            ObjectKind::Commit => Commit::parse(&object.data)?.parents().collect(),
            ObjectKind::Tag => Tag::parse(&object.data)?.object().into_iter().collect(),
            _ => Vec::new(),
        };
        for oid in under {
            if seen.insert(oid) {
                if sought.remove(&oid) {
                    reached.push(oid);
                }
                ahead.push(oid);
            }
        }
    }
    Ok(reached)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fetch names the commits its sender has, a quarter of a million at
    /// the most, and each is looked for in the store before the refs are gone
    /// down for it. A fetch whose time is over looks for none: with none of
    /// them in the store there is nothing else it would have stopped at.
    #[test]
    fn what_a_fetch_says_it_has_is_looked_for_as_long_as_it_is_gone_on_with() {
        let dir = alt_os::temp::tempdir().unwrap();
        alt_testutil::make_repo(dir.path(), "sha1");
        let repo = Repository::discover(dir.path()).unwrap();
        let haves: Vec<ObjectId> = (1..=4u8)
            .map(|n| ObjectId::from_bytes(repo.algo().unwrap(), &[n; 20]).unwrap())
            .collect();
        let req = FetchRequest {
            haves,
            ..FetchRequest::default()
        };
        assert_eq!(reached(&repo, &req, &Until::whenever()).unwrap(), []);
        let over = Until {
            by: Some(std::time::Instant::now()),
            gone: None,
        };
        let stopped = reached(&repo, &req, &over).unwrap_err();
        let late = super::super::until::Stopped::Late;
        assert_eq!(stopped.downcast_ref(), Some(&late), "{stopped}");
    }
}
