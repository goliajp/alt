//! The refs an import leaves in the store: those of the git source, and
//! a ref for each entry of its stash.

use std::path::Path;

use alt_bytes::ByteSlice;
use alt_git_codec::ObjectId;
use alt_git_refs::stash;
use alt_refs::{RefChange, RefStore, RefTarget};
use alt_repo::Repository;
use alt_repo::met::Side;

use crate::ImportError;

fn convert_target(target: alt_git_refs::RefTarget) -> Result<RefTarget, ImportError> {
    Ok(match target {
        alt_git_refs::RefTarget::Direct(oid) => RefTarget::Oid(oid),
        alt_git_refs::RefTarget::Symbolic(name) => RefTarget::Symbolic(
            name.to_str()
                .map_err(|_| ImportError::NonUtf8Ref(name.to_str_lossy().into_owned()))?
                .to_owned(),
        ),
    })
}

/// Every ref the store is to have after an import of `repo`, HEAD first.
/// The stash is the stack git has: an entry ref for each line of the log
/// of `refs/stash`, the oldest first. An entry ref the source has itself,
/// as an older export of a native store left them, is not read.
pub(crate) fn wanted(repo: &Repository) -> Result<Vec<(String, RefTarget)>, ImportError> {
    let git_refs = repo.git_refs().ok_or(ImportError::NotAGitSource)?;
    let mut wanted: Vec<(String, RefTarget)> = Vec::new();
    if let Some(head) = git_refs.read("HEAD")? {
        wanted.push(("HEAD".to_owned(), convert_target(head)?));
    }
    for r in git_refs.iter_refs()? {
        let name = r
            .name
            .to_str()
            .map_err(|_| ImportError::NonUtf8Ref(r.name.to_str_lossy().into_owned()))?
            .to_owned();
        if name == "HEAD" || stash::entry_number(&name).is_some() {
            continue;
        }
        wanted.push((name, convert_target(r.target)?));
    }
    for (number, commit) in stash::read_log(repo.git_dir())?.iter().enumerate() {
        let name = format!("{}{number}", stash::ENTRIES);
        wanted.push((name, RefTarget::Oid(*commit)));
    }
    Ok(wanted)
}

/// The changes that bring `refs` to `wanted`: none for a ref that is
/// there already, and none for one the repository at `git_dir` has as
/// it had it when the store under `alt_dir` last met it: what the store
/// did with such a ref since stands. Where the store keeps nothing of
/// that, what git has is taken.
pub(crate) fn changes(
    refs: &RefStore,
    wanted: &[(String, RefTarget)],
    alt_dir: &Path,
    git_dir: &Path,
) -> Result<Vec<RefChange>, ImportError> {
    let met = alt_repo::met::read(alt_dir, git_dir)?;
    let met = met.as_ref();
    let of_stash = |name: &str| name == stash::TOP || stash::entry_number(name).is_some();
    let mut changes = stashed(refs, wanted, met);
    for (name, new) in wanted.iter().filter(|(name, _)| !of_stash(name)) {
        if met.is_some_and(|met| met.get(name) == Some(new)) {
            continue;
        }
        let (name, new) = (name.clone(), new.clone());
        // a rebase or a bisect of the store has HEAD on a ref of its own,
        // which an export wrote as the commit HEAD is at: that ref takes
        // what the source has there, and HEAD stays on it
        let name = match (name.as_str(), &new, refs.get("HEAD")) {
            ("HEAD", RefTarget::Oid(_), Some(RefTarget::Symbolic(on)))
                if !on.starts_with("refs/") =>
            {
                on.clone()
            }
            _ => name,
        };
        let old = refs.get(&name).cloned();
        if old.as_ref() != Some(&new) {
            changes.push(RefChange {
                name,
                old,
                new: Some(new),
            });
        }
    }
    Ok(changes)
}

/// The changes to the stash: the stack becomes the one git has, and on
/// top of it the entries the store has that git never had. An entry git
/// had when the store last `met` it and has no more is one git dropped,
/// and goes; without that, nothing tells such an entry from one made
/// here, and it stays.
fn stashed(refs: &RefStore, wanted: &[(String, RefTarget)], met: Option<&Side>) -> Vec<RefChange> {
    let entries = |named: &mut dyn Iterator<Item = (&str, &RefTarget)>| {
        let entry = |(name, target): (&str, &RefTarget)| match target {
            RefTarget::Oid(commit) => Some((stash::entry_number(name)?, *commit)),
            RefTarget::Symbolic(_) => None,
        };
        let mut entries: Vec<(u64, ObjectId)> = named.filter_map(entry).collect();
        entries.sort();
        entries
            .into_iter()
            .map(|(_, commit)| commit)
            .collect::<Vec<_>>()
    };
    let mut stack = entries(&mut wanted.iter().map(|(name, target)| (name.as_str(), target)));
    let dropped: Vec<ObjectId> = match met {
        Some(met) => entries(&mut met.iter().map(|(name, target)| (name.as_str(), target))),
        None => Vec::new(),
    };
    let ours = entries(&mut refs.iter());
    let made_here = |commit: &ObjectId| !stack.contains(commit) && !dropped.contains(commit);
    let made_here: Vec<ObjectId> = ours.iter().copied().filter(made_here).collect();
    stack.extend(made_here);
    let mut changes = Vec::new();
    let mut set = |name: String, new: Option<RefTarget>| {
        let old = refs.get(&name).cloned();
        if old != new {
            changes.push(RefChange { name, old, new });
        }
    };
    for number in 0..stack.len().max(ours.len()) {
        let entry = stack.get(number).map(|commit| RefTarget::Oid(*commit));
        set(format!("{}{number}", stash::ENTRIES), entry);
    }
    set(
        stash::TOP.to_owned(),
        stack.last().map(|commit| RefTarget::Oid(*commit)),
    );
    changes
}
