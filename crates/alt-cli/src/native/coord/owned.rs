//! Whose a repository is. Its configuration names programs that are run
//! and files that are read, so one that belongs to another user is not
//! opened, as by git, unless the configuration of the user or of the
//! system names it in `safe.directory`.

use std::path::Path;

use super::super::*;

use alt_repo::owned::{dubious, looked_at};

fn owned_or_safe(tree: &Path, paths: &[&Path]) -> Res<bool> {
    Ok(alt_repo::owned::owned_or_safe(
        &crate::git_settings::var,
        tree,
        paths,
    )?)
}

/// Refuses the repository whose working tree is `tree` where one of
/// `paths`, its working tree and its control directory, belongs to a
/// user other than the one the command runs as.
pub(super) fn ensure_owned(tree: &Path, paths: &[&Path]) -> Res<()> {
    if owned_or_safe(tree, paths)? {
        return Ok(());
    }
    Err(said::refused(&dubious(tree), 128))
}

/// Refuses, as [`ensure_owned`] does, the repository whose store is
/// `alt_dir`, for the daemon that is to hold it.
pub fn ensure_store_owned(alt_dir: &Path) -> Res<()> {
    let tree = alt_dir.parent().unwrap_or(alt_dir);
    ensure_owned(tree, &[tree, alt_dir])
}

/// The repository `start` is in, found by its directory alone: a git
/// repository, or a store reached past a refusal. Refused as by
/// [`ensure_owned`].
pub fn discover_owned(start: &Path) -> Res<alt_repo::Repository> {
    let repo = alt_repo::Repository::discover(start)?;
    let (tree, paths) = looked_at(&repo);
    ensure_owned(tree, &paths)?;
    Ok(repo)
}

/// The repository a server serves at `path`, which is the repository
/// itself: none is looked for above it, as git's servers look for none.
/// One that is another user's is refused as by git, which serves it to
/// no fetch and to no push, unless `safe.directory` names its working
/// tree, as for any command, or its control directory, as git's servers
/// want it named. What is wrong goes to the log of the server; the
/// error, which goes to whoever asked, tells nothing of where the
/// repository is.
pub fn discover_served(path: &Path) -> Res<alt_repo::Repository> {
    let repo = match alt_repo::Repository::discover(path) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("{e}");
            return Err(HIDDEN.into());
        }
    };
    let tree = looked_at(&repo).0;
    if path.canonicalize()? != tree.canonicalize()? {
        eprintln!(
            "no repository at {}: one above it is not served",
            path.display()
        );
        return Err(HIDDEN.into());
    }
    ensure_served(&repo)?;
    Ok(repo)
}

/// Refuses `repo`, as [`discover_served`] does, where it is another
/// user's: a server that keeps a repository open asks this again of
/// every request, as git asks it of every request it serves.
pub fn ensure_served(repo: &alt_repo::Repository) -> Res<()> {
    if !alt_repo::owned::served_safe(&crate::git_settings::var, repo)? {
        eprint!("{}", dubious(looked_at(repo).0));
        return Err(HIDDEN.into());
    }
    Ok(())
}

/// What a client that asked for a repository that is not served is told.
const HIDDEN: &str = "access denied or repository not exported";
