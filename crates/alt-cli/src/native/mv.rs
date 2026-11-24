//! `alt mv`: a file or a directory under another name, in the index and
//! in the working tree.
//!
//! The index entries go to the new name as they are, and so do the files,
//! with whatever was changed in them and not staged. A directory goes
//! with every file in it, tracked or not.

use std::os::unix::fs::MetadataExt;

use super::tree_edit::{Put, TreeEdit};
use super::*;

/// How a `mv` goes about it.
#[derive(Clone, Copy, Default)]
pub struct Mv {
    /// a file at the new name is written over
    pub force: bool,
    /// a source that cannot be moved is passed over
    pub skip_errors: bool,
    /// nothing is moved: the renames are only said
    pub dry_run: bool,
}

/// One source and where it goes, both from the root of the tree.
pub struct Renamed {
    pub from: BString,
    pub to: BString,
}

/// One source on its way to where it goes.
struct Moving<'a> {
    from: &'a BString,
    to: &'a BString,
    force: bool,
    /// the file system takes names that differ in case for one
    folds: bool,
}

/// The path `from` has under its own name in the directory `dir`.
fn within(dir: &BString, from: &BString) -> BString {
    let name = from.rsplit(|b| *b == b'/').next().unwrap_or_default();
    let mut to = dir.to_vec();
    if !to.is_empty() {
        to.push(b'/');
    }
    to.extend_from_slice(name);
    BString::from(to)
}

fn beneath(path: &[u8], dir: &[u8]) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.first() == Some(&b'/'))
}

impl NativeRepo<'_> {
    /// `alt mv <source>… <destination>`, as git has it. Returns the
    /// renames it made: the sources in the order they were given, then
    /// the tracked files of each directory among them.
    pub fn mv(&mut self, sources: &[String], destination: &str, how: Mv) -> Res<Vec<Renamed>> {
        let from_root = |given: &str| self.moved_path(given);
        let destination = from_root(destination)?;
        let folds = self.folds()?;
        let into = self.moves_into(sources, &destination, folds)?;
        let index = self.index()?;
        let mut edit = TreeEdit::of(index.clone());
        // the sources as they were given, then what lies in each directory
        let (mut renamed, mut beneath) = (Vec::new(), Vec::new());
        let mut respelled = Vec::new();
        for source in sources {
            let from = from_root(source)?;
            let to = match into {
                true => within(&destination, &from),
                false => destination.clone(),
            };
            let moving = Moving {
                from: &from,
                to: &to,
                force: how.force,
                folds,
            };
            match self.plan_move(&index, &moving, &mut edit) {
                Ok((files, spelling)) => {
                    if spelling {
                        respelled.push((from.clone(), to.clone()));
                    }
                    renamed.push(Renamed { from, to });
                    beneath.extend(files);
                }
                Err(_) if how.skip_errors => {}
                Err(why) => return Err(why),
            }
        }
        if !how.dry_run {
            self.ensure_writable("mv")?;
            self.respell(&respelled)?;
            self.apply_edit("mv", &[], edit)?;
        }
        renamed.extend(beneath);
        Ok(renamed)
    }

    /// Whether the sources go into the directory `destination`, each
    /// under its own name. A directory named in another spelling is the
    /// one to rename, not one to move into.
    fn moves_into(&self, sources: &[String], destination: &BString, folds: bool) -> Res<bool> {
        let itself = match sources {
            [one] if folds => self.one_file(&self.moved_path(one)?, destination)?,
            _ => false,
        };
        let into = destination.is_empty() || (self.abs(destination)?.is_dir() && !itself);
        if sources.len() > 1 && !into {
            return Err(settings::fatal(format!(
                "destination '{destination}' is not a directory"
            )));
        }
        Ok(into)
    }

    /// A path given to `mv`, from the root of the tree.
    fn moved_path(&self, given: &str) -> Res<BString> {
        let path = pathspec::from_root(&self.root, &self.prefix, given)?;
        let outside = || {
            let root = self.root.display();
            settings::fatal(format!("'{given}' is outside repository at '{root}'"))
        };
        Ok(BString::from(path.ok_or_else(outside)?.into_bytes()))
    }

    /// Whether two paths that are not spelled alike reach one file.
    fn one_file(&self, a: &BString, b: &BString) -> Res<bool> {
        let reached = |path: &BString| -> Res<_> {
            let found = self.abs(path)?.symlink_metadata().ok();
            Ok(found.map(|m| (m.dev(), m.ino())))
        };
        Ok(a != b && !b.is_empty() && reached(a)?.is_some() && reached(a)? == reached(b)?)
    }

    /// Asks the file system to show each file under its new spelling.
    /// The file stays the one it is, and the name its directory shows for
    /// it afterwards is the file system's to say; a refusal is told as
    /// git tells it.
    fn respell(&self, respelled: &[(BString, BString)]) -> Res<()> {
        for (from, to) in respelled {
            let renamed = std::fs::rename(self.abs(from)?, self.abs(to)?);
            renamed.map_err(|e| {
                let why = settings::reason(&e);
                settings::fatal(format!("renaming '{from}' failed: {why}"))
            })?;
        }
        Ok(())
    }

    /// Refuses the move as git refuses it where it cannot be made.
    /// Returns whether the source is a directory, and whether the two
    /// names are one file to a file system that ignores case, so that
    /// only the spelling changes.
    fn ensure_movable(&self, index: &Index, moving: &Moving) -> Res<(bool, bool)> {
        let Moving {
            from,
            to,
            force,
            folds,
        } = *moving;
        let refused = |why: &str| -> Res<(bool, bool)> {
            let said = format!("{why}, source={from}, destination={to}");
            Err(settings::fatal(said))
        };
        let found = self.abs(from)?.symlink_metadata();
        let tracked = |path: &BString| index.entries.iter().any(|e| e.path == *path);
        let Ok(found) = found else {
            return refused("bad source");
        };
        if from == to || beneath(to, from) {
            return refused("can not move directory into itself");
        }
        let target = self.abs(to)?;
        let there = target.symlink_metadata();
        let one = |there: &std::fs::Metadata| {
            folds && (there.dev(), there.ino()) == (found.dev(), found.ino())
        };
        let spelling = there.as_ref().is_ok_and(one);
        if found.is_dir() {
            let inside = |e: &&IndexEntry| beneath(&e.path, from);
            if !index.entries.iter().any(|e| inside(&e)) {
                return refused("source directory is empty");
            }
            if there.is_ok() && !spelling {
                return refused("destination already exists");
            }
        } else if !tracked(from) {
            return refused("not under version control");
        } else if index
            .entries
            .iter()
            .any(|e| e.path == *from && e.stage() > 0)
        {
            return refused("conflicted");
        } else if let Ok(there) = &there
            && !spelling
            && (!force || there.is_dir())
        {
            return refused("destination exists");
        }
        if !target.parent().is_some_and(Path::is_dir) {
            return refused("destination directory does not exist");
        }
        Ok((found.is_dir(), spelling))
    }

    /// Adds the move to `edit`, refused as git refuses it where it
    /// cannot be made. Returns the renames of the tracked files beneath
    /// the source where that is a directory, and whether only the
    /// spelling of a name changes: the files then stay where they are
    /// and the edit is of the index alone.
    fn plan_move(
        &self,
        index: &Index,
        moving: &Moving,
        edit: &mut TreeEdit,
    ) -> Res<(Vec<Renamed>, bool)> {
        let (is_dir, spelling) = self.ensure_movable(index, moving)?;
        let (from, to) = (moving.from, moving.to);
        let moved = |path: &[u8]| -> BString {
            let mut name = to.to_vec();
            name.extend_from_slice(&path[from.len()..]);
            BString::from(name)
        };
        // the entries go as they are, stat and flags left behind
        let goes = |e: &IndexEntry| e.path == *from || beneath(&e.path, from);
        let entries: Vec<IndexEntry> = edit.index.iter().filter(|e| goes(e)).cloned().collect();
        edit.index.retain(|e| !goes(e) && e.path != *to);
        let mut beneath = Vec::new();
        for entry in entries {
            let to = moved(&entry.path);
            if is_dir {
                beneath.push(Renamed {
                    from: entry.path.clone(),
                    to: to.clone(),
                });
            }
            let entry = WorkEntry {
                path: to,
                oid: entry.oid,
                mode: entry.mode,
            };
            edit.index.push(stage_entry(&entry, 0));
        }
        beneath.sort_by(|a, b| a.from.cmp(&b.from));
        let files = match (spelling, is_dir) {
            (true, _) => Vec::new(),
            (false, true) => self.files_beneath(from)?,
            (false, false) => vec![from.clone()],
        };
        for file in files {
            let state = self.file_state(&file, true)?.ok_or("file went away")?;
            edit.files.push((moved(&file), Put::Found(state)));
            edit.files.push((file, Put::Nothing));
        }
        Ok((beneath, spelling))
    }
}
