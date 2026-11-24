//! How the directories above a path are spelled, on a file system that
//! takes names differing in case for one. As by git, a checkout takes
//! away all that leaves before it writes: a directory left empty is
//! gone, and is made again under the spelling of the first path written
//! into it, while one that keeps a file keeps its spelling.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

use super::asked::Asked;
use super::*;

/// The names directories list, each read when first asked about and
/// again where a name is missed: one may have come since. A name found
/// missing stays so.
#[derive(Default)]
pub(in crate::native) struct Listed {
    names: HashMap<PathBuf, HashSet<OsString>>,
    missed: HashSet<PathBuf>,
}

impl Listed {
    fn lists(&mut self, dir: &Path, name: &OsStr) -> Res<bool> {
        if self.names.get(dir).is_some_and(|n| n.contains(name)) {
            return Ok(true);
        }
        let at = dir.join(name);
        if self.missed.contains(&at) {
            return Ok(false);
        }
        let mut names = HashSet::new();
        for entry in std::fs::read_dir(dir)? {
            names.insert(entry?.file_name());
        }
        let found = names.contains(name);
        self.names.insert(dir.to_owned(), names);
        if !found {
            self.missed.insert(at);
        }
        Ok(found)
    }
}

fn steps(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.split(|b| *b == b'/')
}

impl NativeRepo<'_> {
    /// Whether a step of `path` that is there, a directory above it or
    /// the file itself, is listed under another spelling of its name,
    /// where names fold.
    pub(in crate::native) fn spelled_otherwise(
        &self,
        path: &[u8],
        listed: &mut Listed,
    ) -> Res<bool> {
        if !self.folds()? || self.steps_above(path)?.is_err() {
            return Ok(false);
        }
        let mut at = self.root.clone();
        for step in steps(path) {
            let name = OsStr::from_bytes(step);
            if at.join(name).symlink_metadata().is_err() {
                return Ok(false);
            }
            if !listed.lists(&at, name)? {
                return Ok(true);
            }
            at.push(name);
        }
        Ok(false)
    }

    /// Takes away, before the first of `written` is written, each file
    /// among them that is reached under another spelling of its name or
    /// of a directory above it, with the directories that leaves empty.
    pub(super) fn clear_respelled<'c>(
        &self,
        written: impl Iterator<Item = &'c FileChange>,
    ) -> Res<()> {
        if !self.folds()? {
            return Ok(());
        }
        let mut listed = Listed::default();
        let plain = |c: &&FileChange| c.after.as_ref().is_some_and(|s| s.mode != 0o160000);
        for change in written.filter(plain) {
            if !self.spelled_otherwise(&change.path, &mut listed)? {
                continue;
            }
            let abs = self.abs(&BString::from(change.path.clone()))?;
            match abs.symlink_metadata() {
                Ok(found) if found.is_dir() => continue,
                Ok(_) => {
                    crate::native::tree_state::wrote_file(&self.root);
                    crate::durable::remove_file(&abs)?;
                    crash_point!("checkout-file:respelled");
                }
                // taken away by the checkout this one finishes
                Err(_) if change.before.is_some() => {}
                Err(_) => continue,
            }
            let mut above = abs.parent();
            while let Some(dir) = above
                && dir.symlink_metadata().is_err()
            {
                above = dir.parent();
            }
            self.prune_empty_dirs(above);
        }
        Ok(())
    }

    /// Whether two of the paths that `changes` leave a file at spell a
    /// directory they share in two ways: which of the two it takes is
    /// then a matter of the order in which they are written.
    pub(super) fn spelled_two_ways(&self, changes: &[&FileChange]) -> Res<bool> {
        if !self.folds()? {
            return Ok(false);
        }
        // the directories of each directory, by the paths that come
        let mut within: HashMap<&[u8], HashSet<&[u8]>> = HashMap::new();
        for change in changes.iter().filter(|c| c.after.is_some()) {
            let path = change.path.as_slice();
            let mut begin = 0;
            for end in (0..path.len()).filter(|at| path[*at] == b'/') {
                let above = &path[..begin.max(1) - 1];
                within.entry(above).or_default().insert(&path[begin..end]);
                begin = end + 1;
            }
        }
        let mut asked = None;
        for names in within.values().filter(|names| names.len() > 1) {
            let lower: HashSet<Vec<u8>> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
            if lower.len() < names.len() {
                return Ok(true);
            }
            if names.iter().all(|name| name.is_ascii()) {
                continue;
            }
            if asked.is_none() {
                asked = Some(Asked::at(&self.root)?);
            }
            let asked = asked.as_ref().expect("just made");
            if asked.grouped(names.iter().copied())?.len() < names.len() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Names each of `changes` that puts a file where none is now, at a
    /// path that `back` lacks, by the path among `names` that is the
    /// same to the file system; a removal of the same checkout that lies
    /// above or beneath such a path is spelled along with it, so that
    /// the checks of the checkout find the one among the paths of the
    /// other.
    pub(super) fn named_as_folded(
        &self,
        changes: &mut [FileChange],
        back: &HashSet<Vec<u8>>,
        names: &[&BString],
    ) -> Res<()> {
        let comes = |c: &FileChange| {
            c.after.as_ref().is_some_and(|s| s.mode != 0o160000) && !back.contains(&c.path)
        };
        if names.is_empty() || !changes.iter().any(comes) {
            return Ok(());
        }
        let mut all: Vec<&[u8]> = names.iter().flat_map(|n| steps(n.as_slice())).collect();
        all.extend(
            changes
                .iter()
                .filter(|c| comes(c))
                .flat_map(|c| steps(&c.path)),
        );
        all.sort();
        all.dedup();
        let groups = Asked::at(&self.root)?.grouped(all.into_iter())?;
        let mut group: HashMap<Vec<u8>, usize> = HashMap::new();
        for (index, names) in groups.iter().enumerate() {
            group.extend(names.iter().map(|name| (name.to_vec(), index)));
        }
        let folded = |path: &[u8]| -> Vec<usize> { steps(path).map(|s| group[s]).collect() };
        let by_fold: HashMap<Vec<usize>, &BString> =
            names.iter().map(|n| (folded(n.as_slice()), *n)).collect();
        let mut renamed: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for change in changes.iter_mut().filter(|c| comes(c)) {
            if let Some(named) = by_fold.get(&folded(&change.path)) {
                renamed.push((change.path.clone(), named.to_vec()));
                change.path = named.to_vec();
            }
        }
        let inside = |path: &[u8], dir: &[u8]| {
            path.len() > dir.len() && path.starts_with(dir) && path[dir.len()] == b'/'
        };
        for change in changes.iter_mut().filter(|c| c.after.is_none()) {
            for (old, new) in &renamed {
                if inside(old, &change.path) {
                    // as many steps of the new path as the removal has
                    let count = steps(&change.path).count();
                    let kept: Vec<&[u8]> = steps(new).take(count).collect();
                    change.path = kept.join(&b'/');
                    break;
                }
                if inside(&change.path, old) {
                    change.path = [new, &change.path[old.len()..]].concat();
                    break;
                }
            }
        }
        Ok(())
    }
}
