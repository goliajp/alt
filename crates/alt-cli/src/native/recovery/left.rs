//! What index changes that were left unsynced wrote beside the object
//! store and the log: the snapshots their intents name, and the indexes.
//!
//! `alt add` leaves what it writes to the kernel by default. The next
//! command that syncs sets a boundary in the log over the ops of such
//! adds; before it does, it syncs the objects stored by then, and what is
//! synced here. A commit therefore answers with the index it was made
//! from on disk, and with the snapshots that an undo or a recovery of the
//! adds before it reads.

use std::collections::BTreeSet;
use std::fs::File;
use std::io;

use super::*;

fn sync_if_there(path: &Path) -> io::Result<()> {
    match File::open(path) {
        Ok(file) => file.sync_all(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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
    let mut snapshots = BTreeSet::new();
    for op in left {
        // an intent that does not read names nothing here: the recovery
        // of that operation reports it
        if let Ok(intent) = decode::<IndexIntent>(&op.payload, PREPARE) {
            snapshots.extend([intent.before, intent.after]);
        }
    }
    let objects = alt_dir.join("recovery/objects");
    let named = |id: &String| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit());
    for id in snapshots.iter().filter(|id| named(id)) {
        sync_if_there(&objects.join(id))?;
    }
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
    for path in indexes.iter().chain(&directories) {
        sync_if_there(path)?;
    }
    Ok(())
}
