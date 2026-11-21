//! An index file that is not a whole index.
//!
//! `alt add` puts the index in place by a rename and syncs nothing, so a
//! power cut can leave the file empty, cut short or filled with zeros.
//! Such a file holds nothing to recover. What takes its place is the last
//! state of the index that the operation log still vouches for:
//!
//! - the index an operation of this workspace left (an index change, a
//!   checkout, a merge that stopped), where the snapshot of it reads and
//!   every blob it names is stored. Later index changes whose snapshot or
//!   blobs went with the power cut are passed over, back to the last
//!   operation that moved a ref of this workspace: the index is never
//!   put back to a state from before that one
//! - else the tree of HEAD
//!
//! A command that writes puts it in place under the workspace lock and
//! says so; one that only reads answers from it and changes no file.

use std::collections::{HashMap, HashSet};

use super::*;

mod left_by;
mod seen;
mod stopped;
use left_by::{Left, left_by};
pub(in crate::native) use seen::FromFile;
use stopped::stopped_at;

/// Where the index that stands in for a torn file comes from.
#[derive(Clone)]
enum Source {
    Operation(OpId),
    StoppedMerge,
    Head,
}

/// The index that stands in for a torn file.
#[derive(Clone)]
pub(in crate::native) struct Restored {
    bytes: Vec<u8>,
    from: Source,
}

fn said(restored: &Restored, written: bool) -> String {
    let state = match &restored.from {
        Source::Operation(id) => format!("the index operation {id} left"),
        Source::StoppedMerge => "the index the merge that stopped left".to_owned(),
        Source::Head => "the tree of HEAD".to_owned(),
    };
    let found = "the index file of this workspace is not a whole index (a power cut leaves it so \
                 when the last `alt add` was not synced)";
    match written {
        true => format!(
            "warning: {found}; it was restored to {state}. What was staged after that is not \
             staged any more: add the files again\n"
        ),
        false => format!(
            "warning: {found}; this shows the workspace with {state}. The next command that \
             writes in the workspace restores the file, and what was staged after that has to \
             be added again\n"
        ),
    }
}

impl NativeRepo<'_> {
    /// The index of the workspace and the bytes of its file; an empty
    /// index and no bytes where there is no file. In place of a file that
    /// is not a whole index: the index that takes its place, with the
    /// bytes it is written as.
    pub(in crate::native) fn read_index(&self) -> Res<(Index, Option<Vec<u8>>)> {
        let (index, bytes, _) = self.read_index_file()?;
        Ok((index, bytes))
    }

    /// [`Self::read_index`], and which file the bytes are those of: none
    /// for an index that stands in for the file. The checksum of a file
    /// noted as seen whole is not computed again ([`seen`]).
    pub(super) fn read_index_file(&self) -> Res<(Index, Option<Vec<u8>>, Option<FromFile>)> {
        use std::io::Read;
        let mut file = match std::fs::File::open(&self.index_path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((index_file::empty_index(), None, None));
            }
            Err(e) => return Err(e.into()),
        };
        let meta = file.metadata()?;
        let mut bytes = Vec::with_capacity(meta.len() as usize + 1);
        file.read_to_end(&mut bytes)?;
        let algo = self.store.algo()?;
        let noted = seen::seen(&self.index_path, &meta);
        let parsed = match noted {
            Some(_) => Index::parse_checked(&bytes, algo),
            None => Index::parse(&bytes, algo),
        };
        match parsed {
            Ok(index) => {
                // by a command that writes: one that only reads changes no file
                if noted.is_none() && self.write_lock.is_some() {
                    self.note_whole(&meta, None);
                }
                let from = FromFile::of(&meta, noted.flatten());
                Ok((index, Some(bytes), Some(from)))
            }
            Err(alt_git_index::IndexError::Torn(_)) => {
                let bytes = self.in_place_of_torn()?;
                Ok((Index::parse(&bytes, algo)?, Some(bytes), None))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Has the command, once it has succeeded, note that the index file
    /// `meta` describes is whole and that the snapshot `id` holds its
    /// bytes, where one is given.
    pub(super) fn note_whole(&self, meta: &std::fs::Metadata, id: Option<&str>) {
        let snapshot = id.and_then(seen::unhex);
        if let Some(note) = seen::note(meta, snapshot) {
            let place = seen::place(&self.index_path);
            *self.store.seen.lock().unwrap() = Some((place, note));
        }
    }

    pub(in crate::native) fn index(&self) -> Res<Index> {
        Ok(self.read_index()?.0)
    }

    /// The bytes of the index that takes the place of the torn file. Under
    /// the workspace lock they are written; a command that holds no lock,
    /// or only reads, goes by them and leaves the file.
    fn in_place_of_torn(&self) -> Res<Vec<u8>> {
        let held = self.restored.borrow().clone();
        let restored = match held {
            Some(restored) => restored,
            None => {
                let found = self.last_whole()?;
                *self.restored.borrow_mut() = Some(found.clone());
                found
            }
        };
        let writes = self.write_lock.is_some();
        if writes {
            durable_write(&self.index_path, &restored.bytes)?;
        }
        // told once that it is passed over, and once that it was restored
        let told = self.told_restored.get();
        let telling = 1 << u8::from(writes);
        if told & telling == 0 {
            self.told_restored.set(told | telling);
            crate::diagnostics::emit(said(&restored, writes).as_bytes())?;
        }
        Ok(restored.bytes)
    }

    fn last_whole(&self) -> Res<Restored> {
        if let Some(found) = self.left_by_an_operation()? {
            return Ok(found);
        }
        let entries = self.head_entries()?;
        let index = Index {
            version: 2,
            entries: entries.iter().map(|w| stage_entry(w, 0)).collect(),
            extensions: Vec::new(),
        };
        Ok(Restored {
            bytes: canonical_index(index, self.store.algo()?),
            from: Source::Head,
        })
    }

    /// The snapshot `id` where it reads as an index that names only
    /// blobs which are stored and are not among `gone`.
    fn whole_snapshot(&self, id: &str, gone: &HashSet<ObjectId>) -> Res<Option<Vec<u8>>> {
        // a snapshot that is missing or does not read is one a power cut took
        let Ok(bytes) = self.read_snapshot(id) else {
            return Ok(None);
        };
        let Ok(index) = Index::parse(&bytes, self.store.algo()?) else {
            return Ok(None);
        };
        for entry in index.entries {
            let gitlink = entry.mode & 0o170000 == 0o160000;
            if !gitlink && (gone.contains(&entry.oid) || !self.store.odb.contains(&entry.oid)?) {
                return Ok(None);
            }
        }
        Ok(Some(bytes))
    }

    /// The latest index an operation of this workspace left that can be
    /// put back; `None` where the tree of HEAD is to be.
    fn left_by_an_operation(&self) -> Res<Option<Restored>> {
        let instance = match std::fs::read_to_string(self.index_path.with_file_name("workspace-id"))
        {
            Ok(instance) => instance,
            // a workspace no operation was recorded for
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let identity = WorkspaceIdentity {
            name: self.workspace.clone(),
            instance,
        };
        // a command that only reads holds the store with the records the
        // next writer cuts off still in it
        let gone = match self.store.odb.is_read_only() {
            true => self.store.odb.unconfirmed_objects()?,
            false => HashSet::new(),
        };
        let stopped = stopped_at(&self.index_path);
        let mut closed = HashMap::new();
        // past an operation that moved a ref of this workspace
        let mut passed = false;
        for op in self.store.refs.oplog().ops()?.iter().rev() {
            if let Some(stopped) = &stopped
                && stopped.after == op.id.to_string()
            {
                if let Some(bytes) = self.whole_snapshot(&stopped.index, &gone)? {
                    let from = Source::StoppedMerge;
                    return Ok(Some(Restored { bytes, from }));
                }
                if passed {
                    return Ok(None);
                }
            }
            match left_by(op, &identity, &mut closed)? {
                Left::Nothing => {}
                Left::Refs => passed = true,
                Left::Index(snapshot, moved) => {
                    let whole = match &snapshot {
                        Some(id) => self.whole_snapshot(id, &gone)?,
                        None => None,
                    };
                    if let Some(bytes) = whole {
                        let from = Source::Operation(op.id);
                        return Ok(Some(Restored { bytes, from }));
                    }
                    if passed || moved {
                        return Ok(None);
                    }
                }
            }
        }
        Ok(None)
    }
}
