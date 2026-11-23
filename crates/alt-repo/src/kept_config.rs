//! The two configuration files of a native store, and what is read and
//! written of them.
//!
//! `git-import/config` is the store's own: `alt config` writes it, and
//! an import never does. `git-import/source/` holds what an import found
//! in the git repository, as it was: its `config`, its `config.worktree`,
//! and the files of the git directory those include. Each import replaces
//! it whole. The store's own file is read on top of the copy, and an
//! export gives the copy back with the store's own variables written
//! into it.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use alt_git_config::{Config, Entry, IncludeContext, Moved, Name, Place};

use crate::RepoError;

const OWN: &str = "git-import/config";
const SOURCE: &str = "git-import/source";

type Read = (Vec<Entry>, Vec<PathBuf>);

fn read_file(path: &Path) -> Result<Option<Vec<u8>>, RepoError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn parsed(bytes: &Option<Vec<u8>>) -> Result<Vec<Entry>, RepoError> {
    Ok(alt_git_config::parse_file(
        bytes.as_deref().unwrap_or_default(),
    )?)
}

/// `config` of a git directory, and after it its `config.worktree` where
/// the first turns `extensions.worktreeConfig` on.
fn with_worktree(
    dir: &Path,
    load: impl Fn(&Path) -> Result<Read, RepoError>,
) -> Result<Read, RepoError> {
    let config = dir.join("config");
    if !config.is_file() {
        return Ok((Vec::new(), vec![config]));
    }
    let (mut entries, mut files) = load(&config)?;
    let read = Config { entries };
    let on = read.get_bool("extensions", None, "worktreeconfig");
    let on = on.transpose()?.unwrap_or(false);
    entries = read.entries;
    let worktree = dir.join("config.worktree");
    if on && worktree.is_file() {
        let (more, from) = load(&worktree)?;
        entries.extend(more);
        files.extend(from);
    }
    Ok((entries, files))
}

/// The configuration of the git directory `dir` itself, includes
/// resolved, with the files it was read from or would be.
pub(crate) fn of_git(dir: &Path, ctx: &IncludeContext) -> Result<Read, RepoError> {
    with_worktree(dir, |file| {
        let (read, files) = Config::load_listing(file, ctx)?;
        Ok((read.entries, files))
    })
}

/// What an import kept of a git repository's configuration under
/// `control`, the `git-import` directory of a store, read as the git
/// repository read it: a relative path one of the files includes leads
/// to the kept copy of a file of the git directory, or into the working
/// tree the store lies in.
pub fn source(control: &Path, ctx: &IncludeContext) -> Result<Read, RepoError> {
    let kept = control.join("source");
    let tree = control.parent().and_then(Path::parent).unwrap_or(control);
    let moved = Moved {
        kept: kept.clone(),
        tree: tree.to_owned(),
    };
    with_worktree(&kept, |file| {
        let (read, files) = Config::load_moved(file, ctx, &moved)?;
        Ok((read.entries, files))
    })
}

/// The configuration of the store whose `git-import` directory is
/// `control`: what an import kept, and on top the store's own file.
pub fn layers(control: &Path, ctx: &IncludeContext) -> Result<Read, RepoError> {
    let (mut entries, mut files) = source(control, ctx)?;
    let own = control.join("config");
    match own.is_file() {
        true => {
            let (read, from) = Config::load_listing(&own, ctx)?;
            entries.extend(read.entries);
            files.extend(from);
        }
        false => files.push(own),
    }
    Ok((entries, files))
}

/// The text of the two files a store reads its settings from before any
/// include: the kept copy first, the store's own on top.
pub(crate) fn plain(alt_dir: &Path) -> Result<Vec<Entry>, RepoError> {
    let mut entries = parsed(&read_file(&alt_dir.join(SOURCE).join("config"))?)?;
    entries.extend(parsed(&read_file(&alt_dir.join(OWN))?)?);
    Ok(entries)
}

/// The `config` a git repository made of the store under `alt_dir` gets:
/// the kept copy, byte for byte where the store's own file sets nothing
/// else, and with each variable it does set written in as `git config`
/// writes one. A store no import left a copy in gives its own file, and
/// one without either gives nothing. Each file passes `through` first.
pub fn for_git(
    alt_dir: &Path,
    through: impl Fn(&[u8]) -> Vec<u8>,
) -> Result<Option<Vec<u8>>, RepoError> {
    let own = read_file(&alt_dir.join(OWN))?.map(|bytes| through(&bytes));
    let Some(kept) = read_file(&alt_dir.join(SOURCE).join("config"))? else {
        return Ok(own);
    };
    Ok(Some(alt_git_config::overlay(
        &through(&kept),
        &parsed(&own)?,
    )?))
}

/// The paths, from the git directory `dir`, of its configuration files:
/// `config`, `config.worktree`, and every file of the directory one of
/// them includes by a relative path, under whatever condition.
fn files_of(dir: &Path) -> Result<BTreeSet<PathBuf>, RepoError> {
    let mut found = BTreeSet::new();
    let mut todo = vec![PathBuf::from("config"), PathBuf::from("config.worktree")];
    while let Some(path) = todo.pop() {
        let Some(bytes) = read_file(&dir.join(&path))? else {
            continue;
        };
        if !found.insert(path.clone()) {
            continue;
        }
        // a file that is no configuration is kept as any other: whoever
        // comes to read it says so
        let Ok(entries) = alt_git_config::parse_file(&bytes) else {
            continue;
        };
        let at = path.parent().unwrap_or(Path::new(""));
        for entry in entries {
            let includes = matches!(entry.section.as_str(), "include" | "includeif");
            let Some(value) = entry.value.filter(|_| includes && entry.key == "path") else {
                continue;
            };
            let value = PathBuf::from(value.to_string());
            if value.is_absolute() || value.starts_with("~") {
                continue;
            }
            if let Place::Kept(inside) = alt_git_config::place(at, &value) {
                todo.push(inside);
            }
        }
    }
    Ok(found)
}

/// Keeps the configuration files of the git directory `dir` in the store
/// under `alt_dir`, in place of what an earlier import kept. The store's
/// own file is not touched.
pub fn take(dir: &Path, alt_dir: &Path) -> Result<(), RepoError> {
    let kept = alt_dir.join(SOURCE);
    let fresh = alt_dir.join("git-import/source.new");
    if fresh.exists() {
        fs::remove_dir_all(&fresh)?;
    }
    let files = files_of(dir)?;
    for path in &files {
        let to = fresh.join(path);
        fs::create_dir_all(to.parent().expect("a file under the directory"))?;
        fs::copy(dir.join(path), to)?;
    }
    if kept.exists() {
        fs::remove_dir_all(&kept)?;
    }
    if !files.is_empty() {
        fs::rename(&fresh, &kept)?;
    }
    Ok(())
}

/// Gives the git directory `dir` the files [`take`] kept besides
/// `config`. Its `config.worktree` has the store's own value written in
/// for each variable it sets itself, which would stand over `config`'s.
pub fn give(alt_dir: &Path, dir: &Path) -> Result<(), RepoError> {
    let kept = alt_dir.join(SOURCE);
    let own = parsed(&read_file(&alt_dir.join(OWN))?)?;
    for path in files_of(&kept)? {
        if path == Path::new("config") {
            continue;
        }
        let mut bytes = fs::read(kept.join(&path))?;
        if path == Path::new("config.worktree") {
            let theirs = alt_git_config::parse_file(&bytes)?;
            let set_there = |e: &&Entry| theirs.iter().any(|t| Name::of(t) == Name::of(e));
            let over: Vec<Entry> = own.iter().filter(set_there).cloned().collect();
            bytes = alt_git_config::overlay(&bytes, &over)?;
        }
        let to = dir.join(&path);
        fs::create_dir_all(to.parent().expect("a file under the directory"))?;
        fs::write(to, bytes)?;
    }
    Ok(())
}
