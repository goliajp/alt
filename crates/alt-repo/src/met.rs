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
//!
//! Neither is told by where it is: another repository may come to be
//! at the path, and another store. Each record names the one it is of
//! in a line of its own, written as a ref no repository can have: the
//! store by the identity of its default workspace, `:store`, and the
//! git repository by the file its directory is and when that was made,
//! `:git`. A record that does not name the one at hand tells nothing.

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

/// The store under `alt_dir` among all stores.
fn store_named(alt_dir: &Path) -> Result<Option<String>, RepoError> {
    match fs::read_to_string(alt_dir.join("workspace-id")) {
        Ok(id) => Ok(Some(id.trim().to_owned()).filter(|id| !id.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The repository at `git_dir` among all that are or were at its path;
/// `None` where the file system does not tell when a directory was made.
fn git_named(git_dir: &Path) -> Result<Option<String>, RepoError> {
    use std::os::unix::fs::MetadataExt;
    let found = fs::metadata(git_dir)?;
    let Ok(made) = found.created() else {
        return Ok(None);
    };
    let made = made.duration_since(std::time::UNIX_EPOCH);
    let made = made.map(|since| since.as_nanos()).unwrap_or_default();
    Ok(Some(format!("{}.{}.{made}", found.dev(), found.ino())))
}

/// The lines of `refs` where they are of the one `named` by the line
/// `mark`; that line is no ref and is left out.
fn of_named(mut side: Side, mark: &str, named: Option<String>) -> Option<Side> {
    let of = side.remove(mark)?;
    (Some(of) == named.map(RefTarget::Symbolic)).then_some(side)
}

fn parsed(refs: &str, file: &'static str) -> Result<Side, RepoError> {
    let corrupt = || RepoError::Format(file);
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

/// The first line of what an export of the store under `alt_dir`
/// leaves: where the store is, which an older alt tells it by.
fn store(alt_dir: &Path) -> Result<Option<String>, RepoError> {
    Ok(named(alt_dir)?.map(|at| format!("store {at}\n")))
}

const STORE: &str = ":store";
const GIT: &str = ":git";

fn mark(of: Option<String>, mark: &str) -> String {
    of.map(|of| format!("@{of} {mark}\n")).unwrap_or_default()
}

/// The repository the part `refs` is of, as [`git_named`] has it.
fn marked(refs: &str) -> Option<String> {
    let of = |line: &str| {
        Some(
            line.strip_prefix('@')?
                .strip_suffix(GIT)?
                .trim_end()
                .to_owned(),
        )
    };
    refs.lines().find_map(of)
}

/// What the repository at `git_dir` had when the store under `alt_dir`
/// last met it; `None` where nothing tells.
pub fn read(alt_dir: &Path, git_dir: &Path) -> Result<Option<Side>, RepoError> {
    // the part of this very repository, wherever it was when it was met:
    // one at its path that is of another tells nothing of this one
    let itself = git_named(git_dir)?;
    let of_itself = |part: &(String, String)| itself.is_some() && marked(&part.1) == itself;
    if let Some((_, refs)) = parts(alt_dir)?.into_iter().find(of_itself) {
        return Ok(of_named(parsed(&refs, MET)?, GIT, itself));
    }
    let left = match fs::read_to_string(git_dir.join(LEFT)) {
        Ok(left) => left,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    // the store is told by what it is, wherever it was when it left
    // this; what another store left is not read
    let ours = store_named(alt_dir)?;
    let line = ours.as_ref().map(|id| format!("@{id} {STORE}"));
    if !line.is_some_and(|line| left.lines().any(|l| l == line)) {
        return Ok(None);
    }
    let refs = left.split_once('\n').map(|(_, refs)| refs);
    Ok(of_named(
        parsed(refs.unwrap_or_default(), LEFT)?,
        STORE,
        ours,
    ))
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
    let itself = git_named(git_dir)?;
    // neither what was at this path nor this repository where it was before
    let other =
        |part: &(String, String)| part.0 != at && !(itself.is_some() && marked(&part.1) == itself);
    let others = parts(alt_dir)?.into_iter().filter(other);
    for (other, refs) in others.filter(|part| Path::new(&part.0).is_dir()) {
        text.push_str(&format!("git {other}\n{refs}"));
    }
    text.push_str(&format!("git {at}\n{}", mark(itself, GIT)));
    text.push_str(&written(refs));
    let (kept, fresh) = (alt_dir.join(MET), alt_dir.join("git-import/met.new"));
    fs::create_dir_all(alt_dir.join("git-import"))?;
    let mut file = fs::File::create(&fresh)?;
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    file.sync_all()?;
    fs::rename(fresh, kept)?;
    Ok(fs::File::open(alt_dir.join("git-import"))?.sync_all()?)
}

/// Leaves `refs` in the repository at `git_dir`, which the store under
/// `alt_dir` exported to, as what that repository has now.
pub fn leave<'r>(
    alt_dir: &Path,
    git_dir: &Path,
    refs: impl Iterator<Item = (&'r str, &'r RefTarget)>,
) -> Result<(), RepoError> {
    match store(alt_dir)? {
        Some(store) => {
            let of = mark(store_named(alt_dir)?, STORE);
            Ok(fs::write(git_dir.join(LEFT), store + &of + &written(refs))?)
        }
        None => Ok(()),
    }
}
