//! `alt mv`: a file or a directory under another name, in the index and
//! in the working tree.
//!
//! The index entries go to the new name as they are, and so do the files,
//! with whatever was changed in them and not staged. A directory goes
//! with every file in it, tracked or not.

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

fn beneath(path: &[u8], dir: &[u8]) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.first() == Some(&b'/'))
}

impl NativeRepo<'_> {
    /// `alt mv <source>… <destination>`, as git has it. Returns the
    /// renames it made: the sources in the order they were given, then
    /// the tracked files of each directory among them.
    pub fn mv(&mut self, sources: &[String], destination: &str, how: Mv) -> Res<Vec<Renamed>> {
        let from_root = |given: &str| -> Res<BString> {
            let path = pathspec::from_root(&self.root, &self.prefix, given)?;
            let outside = || {
                let root = self.root.display();
                settings::fatal(format!("'{given}' is outside repository at '{root}'"))
            };
            Ok(BString::from(path.ok_or_else(outside)?.into_bytes()))
        };
        let destination = from_root(destination)?;
        let into = destination.is_empty() || self.abs(&destination)?.is_dir();
        if sources.len() > 1 && !into {
            return Err(settings::fatal(format!(
                "destination '{destination}' is not a directory"
            )));
        }
        let index = self.index()?;
        let mut edit = TreeEdit::of(index.clone());
        // the sources as they were given, then what lies in each directory
        let (mut renamed, mut beneath) = (Vec::new(), Vec::new());
        for source in sources {
            let from = from_root(source)?;
            let to = match into {
                true => {
                    let name = from.rsplit(|b| *b == b'/').next().unwrap_or_default();
                    let mut to = destination.to_vec();
                    if !to.is_empty() {
                        to.push(b'/');
                    }
                    to.extend_from_slice(name);
                    BString::from(to)
                }
                false => destination.clone(),
            };
            match self.plan_move(&index, &from, &to, how.force, &mut edit) {
                Ok(files) => {
                    renamed.push(Renamed { from, to });
                    beneath.extend(files);
                }
                Err(_) if how.skip_errors => {}
                Err(why) => return Err(why),
            }
        }
        if !how.dry_run {
            self.apply_edit("mv", &[], edit)?;
        }
        renamed.extend(beneath);
        Ok(renamed)
    }

    /// Adds the move of `from` to `to` to `edit`, refused as git refuses
    /// it where it cannot be made. Returns the renames of the tracked
    /// files beneath `from` where that is a directory.
    fn plan_move(
        &self,
        index: &Index,
        from: &BString,
        to: &BString,
        force: bool,
        edit: &mut TreeEdit,
    ) -> Res<Vec<Renamed>> {
        let refused = |why: &str| -> Res<Vec<Renamed>> {
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
        if found.is_dir() {
            let inside = |e: &&IndexEntry| beneath(&e.path, from);
            if !index.entries.iter().any(|e| inside(&e)) {
                return refused("source directory is empty");
            }
            if target.symlink_metadata().is_ok() {
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
        } else if let Ok(there) = target.symlink_metadata() {
            if !force || there.is_dir() {
                return refused("destination exists");
            }
        }
        if !target.parent().is_some_and(Path::is_dir) {
            return refused("destination directory does not exist");
        }
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
            if found.is_dir() {
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
        let files = match found.is_dir() {
            true => self.files_beneath(from)?,
            false => vec![from.clone()],
        };
        for file in files {
            let state = self.file_state(&file, true)?.ok_or("file went away")?;
            edit.files.push((moved(&file), Put::Found(state)));
            edit.files.push((file, Put::Nothing));
        }
        Ok(beneath)
    }
}
