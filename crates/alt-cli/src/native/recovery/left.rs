//! What index changes that were left unsynced wrote beside the object
//! store and the log: the snapshots their intents name, and the indexes.
//!
//! `alt add` leaves what it writes to the kernel by default. The next
//! command that syncs sets a boundary in the log over the ops of such
//! adds; before it does, it syncs the objects stored by then, and what is
//! synced here. A commit therefore answers with the index it was made
//! from on disk, and with the snapshots that an undo or a recovery of the
//! adds before it reads.
//!
//! Those are the two snapshots of the last change of each workspace and
//! of every change that nothing closed. An undo takes back the last
//! operation of the log and no earlier one; a recovery completes or
//! abandons a change that is open; and an index file that is not whole is
//! put back to what the latest change left, which is synced here. The
//! index between two earlier changes is read by none of them once a later
//! one is on disk, so however many adds were left unsynced, the snapshots
//! synced for a workspace are two.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;

use super::*;

/// Syncs the file or directory at `path` where there is one and no other
/// name of the same file was synced before: `synced` holds those. An
/// index is one file with the snapshot of it.
fn sync_if_there(path: &Path, synced: &mut HashSet<(u64, u64)>) -> io::Result<()> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    match synced.insert((meta.dev(), meta.ino())) {
        true => file.sync_all(),
        false => Ok(()),
    }
}

/// The snapshots that something still reads of those the index changes
/// among `left` name: of the last change of each workspace, and of each
/// change that no op among `left` closes.
fn read_later(left: &[alt_oplog::Op]) -> BTreeSet<String> {
    let (mut last, mut open) = (HashMap::new(), HashMap::new());
    for op in left {
        // an intent that does not read names nothing here: the recovery
        // of that operation reports it
        match op.payload.first() {
            Some(&PREPARE) => {
                if let Ok(intent) = decode::<IndexIntent>(&op.payload, PREPARE) {
                    let named = [intent.before, intent.after];
                    last.insert(intent.workspace.instance, named.clone());
                    open.insert(op.id.to_string(), named);
                }
            }
            Some(&kind @ (COMPLETE | ABANDON)) => {
                if let Ok(done) = decode::<Completion>(&op.payload, kind) {
                    open.remove(&done.prepared);
                }
            }
            _ => {}
        }
    }
    last.into_values()
        .chain(open.into_values())
        .flatten()
        .collect()
}

/// Syncs what the index changes among `left`, ops of the store under
/// `alt_dir` that were appended while appends were left unsynced, wrote
/// outside the object store and the log. The index of every workspace is
/// synced: the record that abandons a change does not say whose it was.
pub(in crate::native) fn sync_left(alt_dir: &Path, left: &[alt_oplog::Op]) -> io::Result<()> {
    let changed =
        |op: &alt_oplog::Op| matches!(op.payload.first(), Some(&(PREPARE | COMPLETE | ABANDON)));
    if !left.iter().any(changed) {
        return Ok(());
    }
    let objects = alt_dir.join("recovery/objects");
    let named = |id: &String| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit());
    let snapshots = read_later(left).into_iter().filter(named);
    let snapshots: Vec<_> = snapshots.map(|id| objects.join(id)).collect();
    let mut directories = vec![objects, alt_dir.join("recovery")];
    let mut indexes = vec![alt_dir.join("index")];
    let workspaces = alt_dir.join("workspaces");
    match std::fs::read_dir(&workspaces) {
        Ok(listing) => {
            for entry in listing {
                let workspace = entry?.path();
                indexes.push(workspace.join("index"));
                directories.push(workspace);
            }
            directories.push(workspaces);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    directories.push(alt_dir.to_owned());
    let mut synced = HashSet::new();
    for path in indexes.iter().chain(&snapshots).chain(&directories) {
        sync_if_there(path, &mut synced)?;
    }
    Ok(())
}
