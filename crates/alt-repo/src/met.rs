//! What the refs of a git repository were when the store last met it:
//! at an import from it, and at an export to it. An import tells by it
//! what git changed from what the store changed: a ref git has as it had
//! it then is one git did nothing to.
//!
//! `git-import/met` has a part for each repository imported from: a line
//! `git` and the directory, then a line for each ref, what it names, a
//! space, and its name. What it names is an object id, or `@` and the
//! ref it follows. The part of a repository that is no longer there goes
//! with the next that is written.
//!
//! An export writes nothing into the store: it leaves the same lines in
//! the repository it makes, as `alt-met` in its git directory, under a
//! line `store` and the directory of the store. An import from a repository
//! the store has no part for goes by that file where the store is the
//! one that left it.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use alt_refs::RefTarget;

use crate::RepoError;

const MET: &str = "git-import/met";
const LEFT: &str = "alt-met";

/// The refs by their names.
pub type Side = HashMap<String, RefTarget>;

fn named(git_dir: &Path) -> Result<Option<String>, RepoError> {
    let at = git_dir.canonicalize()?;
    let plain = |at: &&str| !at.contains('\n');
    Ok(at.to_str().filter(plain).map(str::to_owned))
}

/// Every part of the file, by the directory of its repository.
fn parts(alt_dir: &Path) -> Result<Vec<(String, String)>, RepoError> {
    let text = match fs::read_to_string(alt_dir.join(MET)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut parts: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        match (line.strip_prefix("git "), parts.last_mut()) {
            (Some(at), _) => parts.push((at.to_owned(), String::new())),
            (None, Some((_, refs))) => {
                refs.push_str(line);
                refs.push('\n');
            }
            (None, None) => return Err(RepoError::Format("git-import/met")),
        }
    }
    Ok(parts)
}

fn parsed(refs: &str) -> Result<Side, RepoError> {
    let corrupt = || RepoError::Format("git-import/met");
    let mut side = Side::new();
    for line in refs.lines() {
        let (target, name) = line.split_once(' ').ok_or_else(corrupt)?;
        let target = match target.strip_prefix('@') {
            Some(followed) => RefTarget::Symbolic(followed.to_owned()),
            None => RefTarget::Oid(target.parse().map_err(|_| corrupt())?),
        };
        side.insert(name.to_owned(), target);
    }
    Ok(side)
}

fn written<'r>(refs: impl Iterator<Item = (&'r str, &'r RefTarget)>) -> String {
    let mut text = String::new();
    for (name, target) in refs {
        match target {
            RefTarget::Oid(oid) => text.push_str(&format!("{oid} {name}\n")),
            RefTarget::Symbolic(followed) => text.push_str(&format!("@{followed} {name}\n")),
        }
    }
    text
}

/// The line that names the store under `alt_dir` in what an export
/// leaves: by where it is, which every store has.
fn store(alt_dir: &Path) -> Result<Option<String>, RepoError> {
    Ok(named(alt_dir)?.map(|at| format!("store {at}\n")))
}

/// What the repository at `git_dir` had when the store under `alt_dir`
/// last met it; `None` where nothing tells.
pub fn read(alt_dir: &Path, git_dir: &Path) -> Result<Option<Side>, RepoError> {
    let Some(at) = named(git_dir)? else {
        return Ok(None);
    };
    if let Some((_, refs)) = parts(alt_dir)?.into_iter().find(|part| part.0 == at) {
        return Ok(Some(parsed(&refs)?));
    }
    let left = match fs::read_to_string(git_dir.join(LEFT)) {
        Ok(left) => left,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match store(alt_dir)?.and_then(|store| left.strip_prefix(&store)) {
        Some(refs) => Ok(Some(parsed(refs)?)),
        None => Ok(None),
    }
}

/// Keeps `refs` in the store as what the repository at `git_dir`, which
/// was imported from, has now.
pub fn write<'r>(
    alt_dir: &Path,
    git_dir: &Path,
    refs: impl Iterator<Item = (&'r str, &'r RefTarget)>,
) -> Result<(), RepoError> {
    let Some(at) = named(git_dir)? else {
        return Ok(());
    };
    let mut text = String::new();
    let others = parts(alt_dir)?.into_iter();
    for (other, refs) in others.filter(|part| part.0 != at && Path::new(&part.0).is_dir()) {
        text.push_str(&format!("git {other}\n{refs}"));
    }
    text.push_str(&format!("git {at}\n{}", written(refs)));
    let (kept, fresh) = (alt_dir.join(MET), alt_dir.join("git-import/met.new"));
    fs::create_dir_all(alt_dir.join("git-import"))?;
    fs::write(&fresh, text)?;
    Ok(fs::rename(fresh, kept)?)
}

/// Leaves `refs` in the repository at `git_dir`, which the store under
/// `alt_dir` exported to, as what that repository has now.
pub fn leave<'r>(
    alt_dir: &Path,
    git_dir: &Path,
    refs: impl Iterator<Item = (&'r str, &'r RefTarget)>,
) -> Result<(), RepoError> {
    match store(alt_dir)? {
        Some(store) => Ok(fs::write(git_dir.join(LEFT), store + &written(refs))?),
        None => Ok(()),
    }
}
