//! The `require-signed-commits` policy of a push.
//!
//! It holds for every commit the push adds to the history served: one a
//! command's new value reaches and no ref reached before the push. Such a
//! commit need not come in the push's pack; a push without objects that
//! points a ref at a commit lying in the odb unreferenced adds it just the
//! same. A commit refs already reach was let in under the policy of
//! whoever pushed it, and is not looked at again.
//!
//! Every commit of the pack is checked, reached or not, before the store
//! lock is taken: verifying a signature needs the commit's bytes and the
//! trust keys, nothing the lock guards. Under the lock only the walk that
//! tells what the push adds is left, and the check of what it adds from
//! outside the pack.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Mutex;

use alt_cli::native::{Principal, Store};
use alt_git_codec::{Commit, ObjectId, ObjectKind, Tag};
use alt_wire::push::RefUpdate;

use super::pack_ingest::PreparedPack;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// What the policy is checked with, and the commits of the pack, which
/// all passed: the parents and the committer date of each.
pub(super) struct Checked {
    trust: Vec<(String, alt_sign::PublicKey)>,
    pack: HashMap<ObjectId, (Vec<ObjectId>, i64)>,
}

/// Why the commit `oid` does not satisfy the policy, when it does not.
fn refusal(
    trust: &[(String, alt_sign::PublicKey)],
    oid: &ObjectId,
    bytes: &[u8],
) -> Option<String> {
    let parsed = match alt_cli::commit_sign::extract_alt_sig(bytes) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            return Some(format!(
                "commit {oid} missing alt-sig (require-signed-commits)"
            ));
        }
        Err(e) => return Some(format!("commit {oid} alt-sig unreadable: {e}")),
    };
    let Some((_, pubkey)) = trust.iter().find(|(id, _)| id == &parsed.principal) else {
        return Some(format!(
            "commit {oid} signed by '{}' not in trust store",
            parsed.principal
        ));
    };
    pubkey
        .verify(&parsed.canonical, &parsed.sig)
        .is_err()
        .then(|| {
            format!(
                "commit {oid} alt-sig did not verify against trust['{}']",
                parsed.principal
            )
        })
}

fn links(bytes: &[u8]) -> Res<(Vec<ObjectId>, i64)> {
    let commit = Commit::parse(bytes)?;
    Ok((
        commit.parents().collect(),
        commit.committer_date().unwrap_or(0),
    ))
}

/// Checks the commits of `prepared` when the policy of `principal` asks
/// for signed commits: `None` when it does not, the reason to refuse the
/// push when one of them fails. The store lock is held only to read the
/// policy and the trust keys.
pub(super) fn check_pack(
    store: &Mutex<Store>,
    principal: &Principal,
    prepared: Option<&PreparedPack>,
) -> Res<Option<Result<Checked, String>>> {
    let trust = {
        let guard = store.lock().unwrap();
        if !guard.capabilities_for(principal).require_signed_commits {
            return Ok(None);
        }
        guard.trust_keys()?
    };
    let mut pack = HashMap::new();
    if let Some((ip, order)) = prepared {
        for &(offset, i) in order {
            let obj = ip.read_at(offset)?;
            if obj.kind != ObjectKind::Commit {
                continue;
            }
            let oid = ip.idx().oid_at(i);
            if let Some(reason) = refusal(&trust, &oid, &obj.data) {
                return Ok(Some(Err(reason)));
            }
            match links(&obj.data) {
                Ok(links) => pack.insert(oid, links),
                Err(e) => return Ok(Some(Err(format!("commit {oid} unreadable: {e}")))),
            };
        }
    }
    Ok(Some(Ok(Checked { trust, pack })))
}

const ADDED: u8 = 1;
const SERVED: u8 = 2;

/// The commit `oid` names, through any tags; `None` when it names no
/// commit the odb holds.
fn peeled(store: &Store, mut oid: ObjectId) -> Res<Option<ObjectId>> {
    loop {
        match store.odb_get(&oid)? {
            Some(raw) if raw.kind == ObjectKind::Commit => return Ok(Some(oid)),
            Some(raw) if raw.kind == ObjectKind::Tag => match Tag::parse(&raw.data)?.object() {
                Some(target) => oid = target,
                None => return Ok(None),
            },
            _ => return Ok(None),
        }
    }
}

/// A walk from the refs as they are and from the new values of a push,
/// newest commit first, that tells the commits only the push reaches.
struct Walk<'a> {
    store: &'a Store,
    checked: &'a Checked,
    marks: HashMap<ObjectId, u8>,
    queue: BinaryHeap<(i64, ObjectId)>,
    /// queued commits only the push is known to reach
    open: HashSet<ObjectId>,
}

impl Walk<'_> {
    fn links(&self, oid: &ObjectId) -> Res<Option<(Vec<ObjectId>, i64)>> {
        if let Some(known) = self.checked.pack.get(oid) {
            return Ok(Some(known.clone()));
        }
        match self.store.odb_get(oid)? {
            Some(raw) if raw.kind == ObjectKind::Commit => Ok(Some(links(&raw.data)?)),
            _ => Ok(None),
        }
    }

    fn mark(&mut self, oid: ObjectId, mark: u8) -> Res<()> {
        let marks = self.marks.entry(oid).or_default();
        if *marks & mark != 0 {
            return Ok(());
        }
        *marks |= mark;
        let only_added = *marks == ADDED;
        let Some((_, date)) = self.links(&oid)? else {
            return Ok(());
        };
        self.queue.push((date, oid));
        match only_added {
            true => self.open.insert(oid),
            false => self.open.remove(&oid),
        };
        Ok(())
    }

    /// The commits taken off the queue while only the push reached them.
    /// The walk ends once every queued commit is one the refs reach.
    fn added(&mut self) -> Res<Vec<ObjectId>> {
        let mut added = Vec::new();
        while !self.open.is_empty() {
            let Some((_, oid)) = self.queue.pop() else {
                break;
            };
            let marks = self.marks[&oid];
            if self.open.remove(&oid) {
                added.push(oid);
            }
            let Some((parents, _)) = self.links(&oid)? else {
                continue;
            };
            for parent in parents {
                for mark in [ADDED, SERVED] {
                    if marks & mark != 0 {
                        self.mark(parent, mark)?;
                    }
                }
            }
        }
        // commit dates out of order can have a commit come off the queue
        // before the walk from the refs got to it
        added.retain(|oid| self.marks[oid] == ADDED);
        Ok(added)
    }
}

/// Whether the refs reach all of `commits`: every commit under them is
/// read until that is known.
fn served(store: &Store, tips: &[ObjectId], commits: &[ObjectId]) -> Res<bool> {
    let mut wanted: HashSet<ObjectId> = commits.iter().copied().collect();
    let mut seen = HashSet::new();
    let mut pending = tips.to_vec();
    while let Some(oid) = pending.pop() {
        if !seen.insert(oid) {
            continue;
        }
        wanted.remove(&oid);
        if wanted.is_empty() {
            return Ok(true);
        }
        if let Some(raw) = store.odb_get(&oid)?
            && raw.kind == ObjectKind::Commit
        {
            pending.extend(links(&raw.data)?.0);
        }
    }
    Ok(false)
}

/// The reason to refuse `updates` when they add a commit to the history
/// that fails the policy. Called under the store lock with the pack in
/// the odb and the refs not yet moved.
pub(super) fn added_block(
    store: &mut Store,
    checked: &Checked,
    updates: &[RefUpdate],
) -> Res<Option<String>> {
    let tips: Vec<ObjectId> = store
        .refs_mut()
        .iter()
        .filter_map(|(_, target)| match target {
            alt_refs::RefTarget::Oid(oid) => Some(*oid),
            alt_refs::RefTarget::Symbolic(_) => None,
        })
        .collect();
    let store = &*store;
    let mut walk = Walk {
        store,
        checked,
        marks: HashMap::new(),
        queue: BinaryHeap::new(),
        open: HashSet::new(),
    };
    let mut served_tips = Vec::new();
    for tip in tips {
        if let Some(commit) = peeled(store, tip)? {
            walk.mark(commit, SERVED)?;
            served_tips.push(commit);
        }
    }
    for new in updates.iter().filter_map(|update| update.new) {
        if let Some(commit) = peeled(store, new)? {
            walk.mark(commit, ADDED)?;
        }
    }
    let mut failed = Vec::new();
    for oid in walk.added()? {
        if checked.pack.contains_key(&oid) {
            continue;
        }
        let Some(raw) = store.odb_get(&oid)? else {
            continue;
        };
        if let Some(reason) = refusal(&checked.trust, &oid, &raw.data) {
            failed.push((oid, reason));
        }
    }
    // only a commit that fails is worth the whole history: the walk above
    // may have taken it for added when the refs do reach it
    for (oid, reason) in failed {
        if !served(store, &served_tips, &[oid])? {
            return Ok(Some(reason));
        }
    }
    Ok(None)
}
