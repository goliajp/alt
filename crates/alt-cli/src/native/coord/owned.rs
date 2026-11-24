//! Whose a repository is. Its configuration names programs that are run
//! and files that are read, so one that belongs to another user is not
//! opened, as by git, unless the configuration of the user or of the
//! system names it in `safe.directory`.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::super::*;

/// Whether each of `paths`, the working tree `tree` of a repository and
/// its control directory, belongs to the user the command runs as, or
/// the configuration names `tree` safe.
fn owned_or_safe(tree: &Path, paths: &[&Path]) -> Res<bool> {
    let me = alt_os::sys::geteuid();
    // run through sudo, root may open what the user who called it owns
    let through = std::env::var("SUDO_UID").ok().filter(|_| me == 0);
    let through = through.and_then(|uid| uid.parse::<u32>().ok());
    let mut mine = true;
    for path in paths {
        let owner = path.symlink_metadata()?.uid();
        mine &= owner == me || Some(owner) == through;
    }
    Ok(mine || listed_safe(tree)?)
}

/// What git says of a repository at `tree` that is another user's.
fn dubious(tree: &Path) -> String {
    let at = tree.display();
    format!(
        "fatal: detected dubious ownership in repository at '{at}'\n\
         To add an exception for this directory, call:\n\n\
         \tgit config --global --add safe.directory {at}\n"
    )
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

/// The parts of a repository found whose owner is looked at: its
/// working tree, where it has one, and its control directory.
fn looked_at(repo: &alt_repo::Repository) -> (&Path, Vec<&Path>) {
    let control = repo.git_dir();
    match repo.work_tree() {
        Some(tree) => (tree, vec![tree, control]),
        None => (control, vec![control]),
    }
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
    let hidden = "access denied or repository not exported";
    let repo = match alt_repo::Repository::discover(path) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("{e}");
            return Err(hidden.into());
        }
    };
    let (tree, paths) = looked_at(&repo);
    if path.canonicalize()? != tree.canonicalize()? {
        eprintln!(
            "no repository at {}: one above it is not served",
            path.display()
        );
        return Err(hidden.into());
    }
    if !owned_or_safe(tree, &paths)? && !listed_safe(repo.git_dir())? {
        eprint!("{}", dubious(tree));
        return Err(hidden.into());
    }
    Ok(repo)
}

/// Whether `safe.directory`, in the configuration of the system and of
/// the user, names `tree`. `*` names every directory, and an empty value
/// takes back what was named before it.
fn listed_safe(tree: &Path) -> Res<bool> {
    let tree = tree.canonicalize()?;
    let mut safe = false;
    for entry in crate::git_settings::outer(None, None)? {
        if entry.section != "safe" || entry.subsection.is_some() || entry.key != "directory" {
            continue;
        }
        let value = entry.value.as_ref().map(|v| v.as_slice());
        safe = match value.unwrap_or_default() {
            b"" => false,
            b"*" => true,
            value => safe || names(value, &tree),
        };
    }
    Ok(safe)
}

/// Whether `named`, a value of `safe.directory`, is the directory
/// `tree`, as git reads the value: `~` is the home of the user, `.` the
/// directory the command runs in, and no other path that does not begin
/// at the root names anything. One that ends in `/*` names every
/// directory beneath it. The path is taken as the file system resolves
/// it, as `tree` is.
fn names(named: &[u8], tree: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let named = Path::new(std::ffi::OsStr::from_bytes(named));
    let named = match (named.strip_prefix("~"), crate::git_settings::home()) {
        (Ok(below), Some(home)) => home.join(below),
        _ => named.to_owned(),
    };
    if named == Path::new(".") {
        let here = std::env::current_dir().and_then(|here| here.canonicalize());
        return here.is_ok_and(|here| here == tree);
    }
    if !named.is_absolute() {
        return false;
    }
    let resolved = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
    match named.file_name().is_some_and(|last| last == "*") {
        true => named
            .parent()
            .map(resolved)
            .is_some_and(|above| tree != above && tree.starts_with(above)),
        false => resolved(&named) == tree,
    }
}
