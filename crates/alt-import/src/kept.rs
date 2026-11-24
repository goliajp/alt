//! What an import keeps of a git repository beside its objects and refs.

use std::fs;
use std::path::Path;

use alt_repo::Repository;

use crate::ImportError;

/// Keeps the configuration of `repo` as it is written, its attributes,
/// which decide how files are checked out, and the logs of its refs.
/// `last` is the last op of the store once the refs are brought.
/// `ours` are the logs the store had of its refs before. `met` are the
/// refs the repository has, kept for the next import to tell by what
/// git changed since.
pub(crate) fn keep(
    repo: &Repository,
    alt_dir: &Path,
    last: Option<String>,
    ours: alt_repo::reflog::Carried,
    met: &[(String, alt_refs::RefTarget)],
) -> Result<(), ImportError> {
    alt_repo::kept_config::take(repo.git_dir(), alt_dir)?;
    let attributes = repo.git_dir().join("info/attributes");
    if attributes.is_file() {
        let to = alt_dir.join("git-import/info");
        fs::create_dir_all(&to)?;
        fs::copy(&attributes, to.join("attributes"))?;
    }
    let ours = (ours, repo.algo()?);
    alt_repo::reflog::take(repo.git_dir(), alt_dir, last, ours)?;
    let met = met.iter().map(|(name, target)| (name.as_str(), target));
    Ok(alt_repo::met::write(alt_dir, repo.git_dir(), met)?)
}

/// The logs the store has of its refs, as they are before the import makes
/// `changes`, each with whether the import moves its ref: one that
/// stays where the store has it keeps its log whole.
pub(crate) fn logs(
    alt_dir: &Path,
    refs: &alt_refs::RefStore,
    changes: &[alt_refs::RefChange],
) -> Result<alt_repo::reflog::Carried, ImportError> {
    let names: Vec<&str> = refs.iter().map(|(name, _)| name).collect();
    let mut logs = alt_repo::reflog::carried(alt_dir, names.into_iter())?;
    let changed = |name: &str| changes.iter().any(|change| change.name == name);
    for (name, _, moved) in &mut logs {
        // a head moves with the branch it is on
        let on = match refs.get(name) {
            Some(alt_refs::RefTarget::Symbolic(on)) => changed(on),
            _ => false,
        };
        *moved = on || changed(name);
    }
    Ok(logs)
}
