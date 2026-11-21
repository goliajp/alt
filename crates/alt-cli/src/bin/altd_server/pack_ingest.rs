//! What a push brings into the server odb, and what its refs may point at.

use std::sync::Mutex;

use alt_cli::native::Store;
use alt_git_codec::{HashAlgo, ObjectId};
use alt_wire::push::RefUpdate;

/// A pack indexed and ready to replay (file-system reads done, lock not
/// yet taken). The second tuple element is the order of `(offset, idx)`
/// pairs so the put loop visits records in file order.
pub(super) type PreparedPack = (alt_git_pack::IndexedPack, Vec<(u64, u32)>);

/// Index the pack already on disk at `path` (no store lock needed —
/// `index_pack` walks the file and builds the .idx, a CPU + disk task
/// orthogonal to in-process odb state). The returned (`IndexedPack`,
/// offset order) is paired with [`apply_pack_into_store`] which takes
/// the store lock and replays the puts.
pub(super) fn prepare_pack_for_ingest(
    path: &std::path::Path,
) -> Result<PreparedPack, Box<dyn std::error::Error>> {
    let indexed = alt_git_pack::index_pack(path, HashAlgo::Sha1, true)?;
    let ip = alt_git_pack::IndexedPack::open(&indexed.pack_path, HashAlgo::Sha1)?;
    let idx = ip.idx();
    let mut order: Vec<(u64, u32)> = (0..idx.len())
        .map(|i| (idx.offset_at(i).expect("idx in range"), i))
        .collect();
    order.sort_unstable();
    Ok((ip, order))
}

/// Apply a prepared pack into the server odb under an already-held
/// store guard, returning the list of new commit oids. Holding the
/// guard across the whole put loop is what makes the checkpoint /
/// rewind window indivisible against concurrent receive-pack flows:
/// no other writer can advance `appended_lens` between the surrounding
/// `checkpoint()` calls, so a downstream rewind is guaranteed to drop
/// exactly this push's bytes (and only this push's).
pub(super) fn apply_pack_into_store(
    store: &mut Store,
    ip: &alt_git_pack::IndexedPack,
    order: &[(u64, u32)],
) -> Result<Vec<ObjectId>, Box<dyn std::error::Error>> {
    let idx = ip.idx();
    let mut new_commits = Vec::new();
    for &(offset, i) in order {
        let obj = ip.read_at(offset)?;
        let oid = idx.oid_at(i);
        if obj.kind == alt_git_codec::ObjectKind::Commit {
            new_commits.push(oid);
        }
        store.odb_mut().put(oid, obj.kind, &obj.data)?;
    }
    store.odb_mut().flush()?;
    Ok(new_commits)
}

/// The reason to refuse `updates` when one of them points a ref at an
/// object the odb does not hold, as it stands after the push's pack.
pub(super) fn absent_tip_block(
    store: &Mutex<Store>,
    updates: &[RefUpdate],
) -> Result<Option<String>, alt_odb::OdbError> {
    let guard = store.lock().unwrap();
    for oid in updates.iter().filter_map(|update| update.new) {
        if !guard.odb().contains(&oid)? {
            return Ok(Some("missing necessary objects".into()));
        }
    }
    Ok(None)
}
