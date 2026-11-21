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
use checkout_recovery::{COMPLETE_CHECKOUT, CheckoutDone, CheckoutIntent, PREPARE_CHECKOUT};

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

/// What an operation says of the index of a workspace.
enum Left {
    /// nothing: another workspace's, or of no workspace
    Nothing,
    /// it moved a ref of the workspace and left the index as it was
    Refs,
    /// it left the index in this snapshot, where that is recorded; and
    /// whether it moved a ref of the workspace
    Index(Option<String>, bool),
}

/// The index a merge that stopped left, kept beside the index: no
/// operation records it.
#[derive(alt_json::ToJson, alt_json::FromJson)]
struct Stopped {
    version: u32,
    /// the snapshot of the index
    index: String,
    /// the last operation of the log when it was written
    after: String,
}

fn stopped_path(index_path: &Path) -> PathBuf {
    index_path.with_file_name("index.stopped")
}

fn stopped_at(index_path: &Path) -> Option<Stopped> {
    let bytes = std::fs::read(stopped_path(index_path)).ok()?;
    let stopped: Stopped = alt_json::from_slice(&bytes).ok()?;
    (stopped.version == 1).then_some(stopped)
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
        let bytes = match std::fs::read(&self.index_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((index_file::empty_index(), None));
            }
            Err(e) => return Err(e.into()),
        };
        let algo = self.store.algo()?;
        match Index::parse(&bytes, algo) {
            Ok(index) => Ok((index, Some(bytes))),
            Err(alt_git_index::IndexError::Torn(_)) => {
                let bytes = self.in_place_of_torn()?;
                Ok((Index::parse(&bytes, algo)?, Some(bytes)))
            }
            Err(e) => Err(e.into()),
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

    /// Keeps `index`, which a merge that stopped has just put in place,
    /// where it is found again should the index file be torn later: no
    /// operation records this index.
    pub(in crate::native) fn keep_stopped(&self, index: Index) -> Res<()> {
        let Some(after) = self.store.refs.head_op() else {
            return Ok(());
        };
        let stopped = Stopped {
            version: 1,
            index: self.snapshot(&canonical_index(index, self.store.algo()?))?,
            after: after.to_string(),
        };
        let bytes = alt_json::to_vec(&stopped)?;
        Ok(durable_write(&stopped_path(&self.index_path), &bytes)?)
    }
}

/// What `op` says of the index of the workspace `identity`. Walked from
/// the end of the log: `closed` gathers the records that close a prepared
/// change, which come after it.
fn left_by(
    op: &alt_oplog::Op,
    identity: &WorkspaceIdentity,
    closed: &mut HashMap<String, u8>,
) -> Res<Left> {
    let id = op.id.to_string();
    Ok(match op.payload.first() {
        Some(&kind @ (COMPLETE | ABANDON)) => {
            let done: Completion = decode(&op.payload, kind)?;
            closed.insert(done.prepared, kind);
            Left::Nothing
        }
        Some(&COMPLETE_CHECKOUT) => {
            let done: CheckoutDone = decode(&op.payload, COMPLETE_CHECKOUT)?;
            closed.insert(done.prepared, COMPLETE_CHECKOUT);
            Left::Nothing
        }
        Some(&PREPARE) => {
            let intent: IndexIntent = decode(&op.payload, PREPARE)?;
            if intent.workspace != *identity {
                return Ok(Left::Nothing);
            }
            // one that was abandoned or never closed left what was there
            let left = match closed.get(&id) {
                Some(&COMPLETE) => intent.after,
                _ => intent.before,
            };
            Left::Index(Some(left), false)
        }
        Some(&PREPARE_CHECKOUT) => {
            let intent: CheckoutIntent = decode(&op.payload, PREPARE_CHECKOUT)?;
            if intent.workspace != *identity {
                return Ok(Left::Nothing);
            }
            Left::Index(Some(checked_out(intent, closed.contains_key(&id))), false)
        }
        Some(&alt_refs::PAYLOAD_REF_TX) => {
            let Some(context) = alt_refs::parse_tx(&op.payload)?.and_then(|tx| tx.context) else {
                return Ok(Left::Nothing);
            };
            let context: RefContext = alt_json::from_slice(&context)?;
            if context.workspace != *identity {
                return Ok(Left::Nothing);
            }
            match (context.checkout, context.effects) {
                (Some(intent), _) => {
                    Left::Index(Some(checked_out(intent, closed.contains_key(&id))), true)
                }
                (None, RefEffects::RefsOnly) => Left::Refs,
                // nothing says what this one did to the index
                (None, _) => Left::Index(None, true),
            }
        }
        _ => Left::Nothing,
    })
}

fn checked_out(intent: CheckoutIntent, completed: bool) -> String {
    match completed {
        true => intent.index_after,
        false => intent.index_before,
    }
}
