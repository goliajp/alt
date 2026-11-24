//! The changes of a checkout that are one file gone from a path and come
//! to be at another as it was: such a file is renamed, and a directory
//! all of whose files go to one other directory is renamed whole. What is
//! renamed keeps its permissions and its times, and is not read.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::super::checkout_recovery::FileChange;
use super::super::*;
use super::Listed;

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

/// Whether a directory in `dir`, at any depth, holds nothing.
fn holds_empty(dir: &Path) -> Res<bool> {
    let mut within = vec![dir.to_owned()];
    while let Some(dir) = within.pop() {
        let mut entries = 0;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            entries += 1;
            if entry.file_type()?.is_dir() {
                within.push(entry.path());
            }
        }
        if entries == 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (a.symlink_metadata(), b.symlink_metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

impl NativeRepo<'_> {
    /// Renames `from` to `to`, where nothing else is; whether it did. One
    /// that would pass a file or a symlink above either path is not
    /// made, nor one the file system cannot make from one of its parts
    /// to another: it is left to be made as any other change.
    fn renamed(&self, from: &[u8], to: &[u8], listed: &mut Listed) -> Res<bool> {
        if !self.may_stand(to)? || !self.beneath_directories(from)? {
            return Ok(false);
        }
        // into a directory spelled otherwise it would keep that spelling
        if self.spelled_otherwise(to, listed)? {
            return Ok(false);
        }
        let (source, target) = (
            self.abs(&BString::from(from))?,
            self.abs(&BString::from(to))?,
        );
        if target.symlink_metadata().is_ok() && !same_file(&source, &target) {
            return Ok(false);
        }
        crash_point!("checkout-move:before");
        self.make_directories_above(to)?;
        match crate::durable::rename(&source, &target) {
            Ok(()) => {
                self.prune_empty_dirs(source.parent());
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Makes those of the `moves` among `changes` it can by renaming, and
    /// returns the paths whose change is made with that. What leaves
    /// without moving is gone by now; a move to beneath a file that
    /// moves itself waits for that one.
    pub(super) fn rename_moved<'c>(
        &self,
        changes: &[&'c FileChange],
        moves: &HashMap<&'c [u8], &'c [u8]>,
    ) -> Res<HashSet<&'c [u8]>> {
        let mut done: HashSet<&'c [u8]> = HashSet::new();
        if moves.is_empty() {
            return Ok(done);
        }
        super::super::tree_state::wrote_file(&self.root);
        let mut listed = Listed::default();
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
            // no directory is renamed to within itself
            let apart = !inside(to, from) && !inside(from, to);
            if files.is_empty() || !apart || !files.iter().all(goes) || expected != files.len() {
                continue;
            }
            // a directory with nothing in it is no file of the checkout:
            // renamed along, it would leave the place git leaves it in
            if holds_empty(&self.abs(&BString::from(from))?)? {
                continue;
            }
            if self.renamed(from, to, &mut listed)? {
                for (gone, come) in moves.iter().filter(|(gone, _)| inside(gone, from)) {
                    done.extend([*gone, *come]);
                }
            }
        }
        let mut waiting: Vec<(&'c [u8], &'c [u8])> = moves
            .iter()
            .filter(|(from, _)| !done.contains(*from))
            .map(|(from, to)| (*from, *to))
            .collect();
        waiting.sort();
        loop {
            let mut blocked = Vec::new();
            for (from, to) in &waiting {
                match self.renamed(from, to, &mut listed)? {
                    true => done.extend([*from, *to]),
                    false => blocked.push((*from, *to)),
                }
            }
            if blocked.len() == waiting.len() {
                return Ok(done);
            }
            waiting = blocked;
        }
    }
}

impl NativeRepo<'_> {
    /// The moves among `changes` to make by renaming: none where two
    /// paths that come spell one directory in two ways, which is then
    /// spelled as by the first of them written.
    pub(super) fn moves_among<'c>(
        &self,
        changes: &[&'c FileChange],
    ) -> Res<HashMap<&'c [u8], &'c [u8]>> {
        Ok(match self.spelled_two_ways(changes)? {
            true => HashMap::new(),
            false => moves_among(changes),
        })
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
