//! A file and a directory whose names differ in case alone, on a file
//! system that takes such names for one. A checkout that takes one away
//! and brings the other reaches the one that leaves by the spelling of
//! the one that comes, and removes it by that spelling. A tree that holds
//! both cannot stand there: it is refused before anything is written,
//! and an import checks out what a clone by git leaves of it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::os::unix::fs::MetadataExt;

use super::asked::Asked;
use super::*;

/// A file, to the file system.
type File = (u64, u64);

/// The path that has each file.
type Files<'p> = HashMap<File, &'p BString>;

/// The paths an import leaves out of the tree, and each group of paths
/// that met, those that stay first.
pub(in crate::native) type LeftOut = (HashSet<Vec<u8>>, Vec<Vec<BString>>);

/// What a checkout is about to do, by its paths.
pub(in crate::native) struct Coming<'a> {
    /// the paths it leaves a file at, each with whether it is a submodule
    pub(in crate::native) come: Vec<(&'a BString, bool)>,
    /// the tracked paths it takes away
    pub(in crate::native) leave: Vec<&'a BString>,
    /// every path the tree holds once it is done
    pub(in crate::native) held: Vec<&'a BString>,
}

impl<'a> Coming<'a> {
    /// The checkout that turns the tree of `old` into that of `target`,
    /// leaving the paths `same` as they are.
    pub(in crate::native) fn of(
        old: &'a [WorkEntry],
        target: &'a [WorkEntry],
        same: &HashSet<&[u8]>,
    ) -> Self {
        let stays: HashSet<&[u8]> = target.iter().map(|e| e.path.as_slice()).collect();
        let changed = target.iter().filter(|e| !same.contains(e.path.as_slice()));
        Coming {
            come: changed.map(|e| (&e.path, e.mode == 0o160000)).collect(),
            leave: old
                .iter()
                .map(|e| &e.path)
                .filter(|p| !stays.contains(p.as_slice()))
                .collect(),
            held: target.iter().map(|e| &e.path).collect(),
        }
    }
}

/// The files of paths that leave, by the spelling under which a path
/// that comes reaches each: the path found, and the tracked path it is.
pub(in crate::native) type Respelled = HashMap<Vec<u8>, BString>;

/// Has each removal among `changes` remove its file by the spelling a
/// path that comes reaches it under: the checks of the checkout, of its
/// undo and of its recovery then find it among the paths of the checkout.
pub(in crate::native) fn respell(changes: &mut [FileChange], respelled: &Respelled) {
    for (found, tracked) in respelled {
        let removal = changes
            .iter_mut()
            .find(|c| c.after.is_none() && c.path == tracked.as_slice());
        if let Some(change) = removal {
            change.path = found.clone();
        }
    }
}

/// Why a path is not checked out beside the path `other` that the tree
/// keeps: one is a file where the other needs a directory of that name.
fn one_name(path: &BString, other: &BString) -> Box<dyn std::error::Error> {
    let why = format!(
        "'{path}' and '{other}' meet in one name here, where names that differ in case are one name: a tree that holds both is not checked out"
    );
    crate::machine::Failure::refusal("precondition_failed", why).into()
}

/// Names of one directory that the file system takes for one, a file
/// among them: the paths that are such files, and the paths beneath such
/// names, as indexes into the paths asked about.
struct Clash {
    files: Vec<usize>,
    beneath: Vec<usize>,
}

/// Sorts `items`, each the rest of a path below one directory and the
/// index of the path, by the name they begin with, and tells in
/// `clashes` where names that are one name have a file among them.
fn sort(
    root: &Path,
    items: Vec<(&[u8], usize)>,
    asked: &mut Option<Asked>,
    clashes: &mut Vec<Clash>,
) -> Res<()> {
    type Named<'p> = (Option<usize>, Vec<(&'p [u8], usize)>);
    let mut names: BTreeMap<&[u8], Named> = BTreeMap::new();
    for (rest, index) in items {
        match rest.iter().position(|b| *b == b'/') {
            Some(at) => {
                let below = &mut names.entry(&rest[..at]).or_default().1;
                below.push((&rest[at + 1..], index));
            }
            None => names.entry(rest).or_default().0 = Some(index),
        }
    }
    let groups = match names.len() {
        0 | 1 => vec![names.keys().copied().collect()],
        _ => {
            if asked.is_none() {
                *asked = Some(Asked::at(root)?);
            }
            let asked = asked.as_ref().expect("just made");
            asked.grouped(names.keys().copied())?
        }
    };
    for group in groups {
        let files: Vec<usize> = group.iter().filter_map(|name| names[name].0).collect();
        let below: Vec<(&[u8], usize)> = group
            .iter()
            .flat_map(|name| names[name].1.clone())
            .collect();
        if group.len() > 1 && !files.is_empty() {
            let beneath = below.iter().map(|(_, index)| *index).collect();
            clashes.push(Clash { files, beneath });
        } else if !below.is_empty() {
            sort(root, below, asked, clashes)?;
        }
    }
    Ok(())
}

impl NativeRepo<'_> {
    /// The file each of `paths` has, for those that have one.
    fn files_of<'p>(&self, paths: &[&'p BString]) -> Res<Files<'p>> {
        let mut files = HashMap::new();
        for path in paths {
            if let Some(file) = self.file_there(path)? {
                files.insert(file, *path);
            }
        }
        Ok(files)
    }

    /// Where a path that comes meets the file of a path that leaves under
    /// another spelling of its name: a file where it needs a directory,
    /// or a directory where it is a file. Refused where the file met is
    /// that of a path the tree keeps.
    pub(in crate::native) fn respelled_by(&self, coming: &Coming) -> Res<Respelled> {
        let mut respelled = Respelled::new();
        if !self.folds()? {
            return Ok(respelled);
        }
        let leave: HashSet<&[u8]> = coming.leave.iter().map(|p| p.as_slice()).collect();
        let mut known: Option<(Files, Files)> = None;
        let mut met = |found: BString, file: File, path: &BString| -> Res<()> {
            if known.is_none() {
                known = Some((self.files_of(&coming.leave)?, self.files_of(&coming.held)?));
            }
            let (leaving, held) = known.as_ref().expect("just read");
            match (leaving.get(&file), held.get(&file)) {
                (Some(left), _) => drop(respelled.insert(found.to_vec(), (*left).clone())),
                (None, Some(other)) if **other != found => return Err(one_name(path, other)),
                _ => {}
            }
            Ok(())
        };
        for (path, submodule) in &coming.come {
            let bytes = path.as_slice();
            let mut below_file = false;
            for at in (0..bytes.len()).filter(|at| bytes[*at] == b'/') {
                let parent = BString::from(bytes[..at].to_vec());
                match self.abs(&parent)?.symlink_metadata() {
                    Ok(found) if found.is_dir() => continue,
                    Ok(found) if !leave.contains(parent.as_slice()) => {
                        met(parent, (found.dev(), found.ino()), path)?;
                    }
                    _ => {}
                }
                below_file = true;
                break;
            }
            if below_file || *submodule {
                continue;
            }
            for inner in self.files_beneath(path)? {
                if leave.contains(inner.as_slice()) {
                    continue;
                }
                if let Some(file) = self.file_there(&inner)? {
                    met(inner, file, path)?;
                }
            }
        }
        Ok(respelled)
    }

    /// Both checks of a checkout that is worked out in one pass.
    pub(in crate::native) fn kept_apart(&self, coming: &Coming) -> Res<Respelled> {
        let respelled = self.respelled_by(coming)?;
        self.ensure_new_apart(coming)?;
        Ok(respelled)
    }

    /// The checks of a merge that stops: it writes its files beside the
    /// conflicts, and puts no file where a directory of its name leaves,
    /// nor a directory where such a file leaves.
    pub(in crate::native) fn ensure_none_respelled(&self, coming: &Coming) -> Res<()> {
        let respelled = self.kept_apart(coming)?;
        let Some((found, tracked)) = respelled.iter().next() else {
            return Ok(());
        };
        let found = BString::from(found.clone());
        let why = format!(
            "'{found}' is the file of '{tracked}' here, where names that differ in case are one name: a merge that stops with conflicts does not put a file and a directory of one name in each other's place"
        );
        Err(crate::machine::Failure::refusal("precondition_failed", why).into())
    }

    /// The clashes among `paths`, which are in the order of an index.
    fn clashes(&self, paths: &[&BString]) -> Res<Vec<Clash>> {
        let items = paths.iter().enumerate();
        let items = items
            .map(|(index, path)| (path.as_slice(), index))
            .collect();
        let mut clashes = Vec::new();
        sort(&self.root, items, &mut None, &mut clashes)?;
        Ok(clashes)
    }

    /// Refuses a checkout that brings two paths of which one is a file
    /// where the other, to the file system, has a file or a directory of
    /// the same name.
    pub(in crate::native) fn ensure_new_apart(&self, coming: &Coming) -> Res<()> {
        if !self.folds()? {
            return Ok(());
        }
        let paths: Vec<&BString> = coming.come.iter().map(|(path, _)| *path).collect();
        match self.clashes(&paths)?.first() {
            Some(clash) => {
                let file = paths[clash.files[0]];
                let others = clash.files[1..].iter().chain(&clash.beneath);
                let other = paths[*others.min().expect("a clash is of two names")];
                Err(one_name(file, other))
            }
            None => Ok(()),
        }
    }

    /// What a clone by git leaves out of the tree of `entries` on a file
    /// system where a file and a directory in it have one name: of such
    /// names the path written last stays, with the paths beneath it. The
    /// paths left out, and each group of paths that met, those that stay
    /// first.
    pub(in crate::native) fn left_out(&self, entries: &[WorkEntry]) -> Res<LeftOut> {
        let mut out: HashSet<Vec<u8>> = HashSet::new();
        let mut groups = Vec::new();
        if !self.folds()? {
            return Ok((out, groups));
        }
        loop {
            let live = entries.iter().map(|e| &e.path);
            let live: Vec<&BString> = live.filter(|p| !out.contains(p.as_slice())).collect();
            let clashes = self.clashes(&live)?;
            let mut clashes = clashes.iter().filter(|c| !c.beneath.is_empty()).peekable();
            if clashes.peek().is_none() {
                return Ok((out, groups));
            }
            for clash in clashes {
                let last = *clash.files.iter().max().expect("a clash has a file");
                let after = clash.beneath.iter().filter(|index| **index > last);
                let mut stay: Vec<usize> = after.copied().collect();
                if stay.is_empty() {
                    stay.push(last);
                }
                let all = clash.files.iter().chain(&clash.beneath);
                let gone: Vec<usize> = all.copied().filter(|i| !stay.contains(i)).collect();
                out.extend(gone.iter().map(|index| live[*index].to_vec()));
                let group = stay.iter().chain(&gone).map(|index| live[*index].clone());
                groups.push(group.collect());
            }
        }
    }
}
