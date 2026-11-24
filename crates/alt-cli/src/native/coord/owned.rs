//! Whose a repository is. Its configuration names programs that are run
//! and files that are read, so one that belongs to another user is not
//! opened, as by git, unless the configuration of the user or of the
//! system names it in `safe.directory`.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::super::*;

/// Refuses the repository whose working tree is `tree` where one of
/// `paths`, its working tree and its control directory, belongs to a
/// user other than the one the command runs as.
pub(super) fn ensure_owned(tree: &Path, paths: &[&Path]) -> Res<()> {
    let me = alt_os::sys::geteuid();
    // run through sudo, root may open what the user who called it owns
    let through = std::env::var("SUDO_UID").ok().filter(|_| me == 0);
    let through = through.and_then(|uid| uid.parse::<u32>().ok());
    let mut mine = true;
    for path in paths {
        let owner = path.symlink_metadata()?.uid();
        mine &= owner == me || Some(owner) == through;
    }
    if mine || listed_safe(tree)? {
        return Ok(());
    }
    let at = tree.display();
    let said = format!(
        "fatal: detected dubious ownership in repository at '{at}'\n\
         To add an exception for this directory, call:\n\n\
         \tgit config --global --add safe.directory {at}\n"
    );
    Err(said::refused(&said, 128))
}

/// [`ensure_owned`] of a repository found by its directory alone: a git
/// repository, or a store reached past a refusal.
pub fn ensure_repository_owned(repo: &alt_repo::Repository) -> Res<()> {
    let control = repo.git_dir();
    match repo.work_tree() {
        Some(tree) => ensure_owned(tree, &[tree, control]),
        None => ensure_owned(control, &[control]),
    }
}

/// Whether `safe.directory`, in the configuration of the system and of
/// the user, names `tree`: itself, `*`, or a directory above it
/// followed by `/*`. An empty value takes back what was named before it.
fn listed_safe(tree: &Path) -> Res<bool> {
    let tree = tree.canonicalize()?;
    let tree = tree.as_os_str().as_encoded_bytes();
    let mut safe = false;
    for entry in crate::git_settings::outer(None, None)? {
        if entry.section != "safe" || entry.subsection.is_some() || entry.key != "directory" {
            continue;
        }
        let value = entry
            .value
            .as_ref()
            .map(|v| v.as_slice())
            .unwrap_or_default();
        safe = match value {
            b"" => false,
            b"*" => true,
            value => {
                let within = value.strip_suffix(b"/*").is_some_and(|above| {
                    tree.starts_with(above) && tree.get(above.len()) == Some(&b'/')
                });
                safe || within || value.strip_suffix(b"/").unwrap_or(value) == tree
            }
        };
    }
    Ok(safe)
}
