//! What the refs of a git repository were when the store last met it:
//! at an import from it, and at an export to it. An import tells by it
//! what git changed from what the store changed: a ref git has as it had
//! it then is one git did nothing to.
//!
//! `git-import/met` has a part for each repository: a line `git` and
//! the directory, then a line for each ref, what it names, a space, and
//! its name. What it names is an object id, or `@` and the ref it
//! follows. The part of a repository that is no longer there goes with
//! the next that is written.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use alt_refs::RefTarget;

use crate::RepoError;

const MET: &str = "git-import/met";

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

/// What the repository at `git_dir` had when the store under `alt_dir`
/// last met it; `None` where the store keeps nothing of it.
pub fn read(alt_dir: &Path, git_dir: &Path) -> Result<Option<Side>, RepoError> {
    let Some(at) = named(git_dir)? else {
        return Ok(None);
    };
    let Some((_, refs)) = parts(alt_dir)?.into_iter().find(|part| part.0 == at) else {
        return Ok(None);
    };
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
    Ok(Some(side))
}

/// Keeps `refs` as what the repository at `git_dir` has now.
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
    text.push_str(&format!("git {at}\n"));
    for (name, target) in refs {
        match target {
            RefTarget::Oid(oid) => text.push_str(&format!("{oid} {name}\n")),
            RefTarget::Symbolic(followed) => text.push_str(&format!("@{followed} {name}\n")),
        }
    }
    let (kept, fresh) = (alt_dir.join(MET), alt_dir.join("git-import/met.new"));
    fs::create_dir_all(alt_dir.join("git-import"))?;
    fs::write(&fresh, text)?;
    Ok(fs::rename(fresh, kept)?)
}
