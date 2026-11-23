//! The changes of a checkout that are one file gone from a path and come
//! to be at another as it was: such a file is renamed, and a directory
//! all of whose files go to one other directory is renamed whole. What is
//! renamed keeps its permissions and its times, and is not read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::super::checkout_recovery::FileChange;
use super::super::*;

/// The directories a move of `from` to `to` is a rename of, where the
/// two paths end alike: `d/s/y` to `e/s/y` moves `d` to `e`.
fn directories<'c>(from: &'c [u8], to: &'c [u8]) -> Option<(&'c [u8], &'c [u8])> {
    let (mut f, mut t) = (from.len(), to.len());
    loop {
        let name = |path: &'c [u8], end: usize| {
            let begin = path[..end].iter().rposition(|b| *b == b'/')?;
            Some((begin, &path[begin + 1..end]))
        };
        match (name(from, f), name(to, t)) {
            (Some((fb, a)), Some((tb, b))) if a == b => (f, t) = (fb, tb),
            _ => break,
        }
    }
    (f < from.len()).then_some((&from[..f], &to[..t]))
}

fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (a.symlink_metadata(), b.symlink_metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Renames `from` to `to`, where nothing else is; whether it did. A
/// rename the file system cannot make from one of its parts to another
/// is left to be made as any other change.
fn renamed(from: &Path, to: &Path) -> Res<bool> {
    if to.symlink_metadata().is_ok() && !same_file(from, to) {
        return Ok(false);
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match crate::durable::rename(from, to) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => Ok(false),
        Err(e) => Err(e.into()),
    }
}

impl NativeRepo<'_> {
    /// Makes those of `changes` that move a file by renaming it, and
    /// returns the paths whose change is made with that.
    pub(super) fn rename_moved<'c>(&self, changes: &[&'c FileChange]) -> Res<HashSet<&'c [u8]>> {
        let moves = moves_among(changes);
        let mut done: HashSet<&'c [u8]> = HashSet::new();
        if moves.is_empty() {
            return Ok(done);
        }
        super::super::tree_state::wrote_file(&self.root);
        let abs = |path: &[u8]| -> Res<PathBuf> { self.abs(&BString::from(path)) };
        // a directory first, the shortest before those within it
        let mut whole: Vec<(&[u8], &[u8])> = moves
            .iter()
            .filter_map(|(from, to)| directories(from, to))
            .collect();
        whole.sort();
        whole.dedup();
        for (from, to) in whole {
            let inside = |path: &[u8], dir: &[u8]| {
                path.len() > dir.len() && path.starts_with(dir) && path[dir.len()] == b'/'
            };
            // every file in the directory goes along, to the same place
            let files = self.files_beneath(&BString::from(from))?;
            let goes = |file: &BString| {
                let there = [to, &file[from.len()..]].concat();
                moves
                    .get(file.as_slice())
                    .is_some_and(|at| **at == there[..])
                    && !done.contains(file.as_slice())
            };
            let others = changes.iter().filter(|c| inside(&c.path, to));
            let expected = others.filter(|c| c.after.is_some()).count();
            if files.is_empty() || !files.iter().all(goes) || expected != files.len() {
                continue;
            }
            if renamed(&abs(from)?, &abs(to)?)? {
                for (gone, come) in moves.iter().filter(|(gone, _)| inside(gone, from)) {
                    done.extend([*gone, *come]);
                }
                self.prune_empty_dirs(abs(from)?.parent());
            }
        }
        for (from, to) in &moves {
            if !done.contains(from) && renamed(&abs(from)?, &abs(to)?)? {
                done.extend([*from, *to]);
                self.prune_empty_dirs(abs(from)?.parent());
            }
        }
        Ok(done)
    }
}

/// The moves among `changes`: a path whose file goes, with a path where
/// nothing was and the same file comes to be. Of several such paths the
/// one that ends most like the first is taken.
fn moves_among<'c>(changes: &[&'c FileChange]) -> HashMap<&'c [u8], &'c [u8]> {
    let file = |c: &FileChange| c.before.as_ref().or(c.after.as_ref()).map(|s| s.mode);
    let plain = |c: &&&'c FileChange| file(c).is_some_and(|mode| mode != 0o160000);
    let gone = changes.iter().filter(plain).filter(|c| c.after.is_none());
    let mut come: Vec<&'c FileChange> = changes
        .iter()
        .filter(plain)
        .filter(|c| c.before.is_none())
        .copied()
        .collect();
    let alike = |a: &[u8], b: &[u8]| {
        a.iter()
            .rev()
            .zip(b.iter().rev())
            .take_while(|(a, b)| a == b)
            .count()
    };
    let mut moves = HashMap::new();
    for from in gone {
        let same = |c: &&FileChange| c.after == from.before;
        let Some(to) = come
            .iter()
            .copied()
            .filter(same)
            .max_by_key(|c| alike(&c.path, &from.path))
        else {
            continue;
        };
        come.retain(|c| c.path != to.path);
        moves.insert(from.path.as_slice(), to.path.as_slice());
    }
    moves
}
