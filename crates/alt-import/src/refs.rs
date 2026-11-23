//! The refs an import leaves in the store: those of the git source, and
//! a ref for each entry of its stash.

use alt_bytes::ByteSlice;
use alt_git_refs::stash;
use alt_refs::{RefChange, RefStore, RefTarget};
use alt_repo::Repository;

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

/// Every ref the store is to have after an import of `repo`, HEAD first,
/// and whether the entries of the stash among them are read from the log
/// of `refs/stash`. A source that has entry refs of its own, as an export
/// of a native store does, has its stack in those.
pub(crate) fn wanted(repo: &Repository) -> Result<(Vec<(String, RefTarget)>, bool), ImportError> {
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
        if name == "HEAD" {
            continue; // already handled
        }
        wanted.push((name, convert_target(r.target)?));
    }
    let own = wanted
        .iter()
        .any(|(name, _)| stash::entry_number(name).is_some());
    let logged = match own {
        true => Vec::new(),
        false => stash::read_log(repo.git_dir())?,
    };
    for (number, commit) in logged.iter().enumerate() {
        let name = format!("{}{number}", stash::ENTRIES);
        wanted.push((name, RefTarget::Oid(*commit)));
    }
    Ok((wanted, !logged.is_empty()))
}

/// The changes that bring `refs` to `wanted`: none for a ref that is
/// there already. Where the stash was read from its log, an entry the
/// store has and the log has no longer goes.
pub(crate) fn changes(
    refs: &RefStore,
    wanted: Vec<(String, RefTarget)>,
    stack_from_log: bool,
) -> Vec<RefChange> {
    let gone = |(name, target): (&str, &RefTarget)| {
        let entry = stash::entry_number(name).is_some();
        let kept = wanted.iter().any(|(wanted, _)| wanted == name);
        (stack_from_log && entry && !kept).then(|| RefChange {
            name: name.to_owned(),
            old: Some(target.clone()),
            new: None,
        })
    };
    let mut changes: Vec<RefChange> = refs.iter().filter_map(gone).collect();
    for (name, new) in wanted {
        let old = refs.get(&name).cloned();
        if old.as_ref() != Some(&new) {
            changes.push(RefChange {
                name,
                old,
                new: Some(new),
            });
        }
    }
    changes
}
