//! What a push brings into the server odb, and what its refs may point at.

use alt_cli::native::{Principal, Store};
use alt_git_codec::{HashAlgo, ObjectId};
use alt_wire::push::RefUpdate;

use super::{WriteCoordinator, commit_ref_updates, require_signed_commits_block};

/// A pack indexed and ready to replay (file-system reads done, lock not
/// yet taken). The second tuple element is the order of `(offset, idx)`
/// pairs so the put loop visits records in file order.
pub(super) type PreparedPack = (alt_git_pack::IndexedPack, Vec<(u64, u32)>);

/// Index the pack already on disk at `path` (no store lock needed —
/// `index_pack` walks the file and builds the .idx, a CPU + disk task
/// orthogonal to in-process odb state). The returned (`IndexedPack`,
/// offset order) goes to [`land`], which takes the store lock and
/// replays the puts.
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
/// store guard, returning the list of new commit oids.
fn apply_pack_into_store(
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
fn absent_tip_block(
    store: &Store,
    updates: &[RefUpdate],
) -> Result<Option<String>, alt_odb::OdbError> {
    for oid in updates.iter().filter_map(|update| update.new) {
        if !store.odb().contains(&oid)? {
            return Ok(Some("missing necessary objects".into()));
        }
    }
    Ok(None)
}

/// How the unpack went and, when the refs moved, the ticket to wait on
/// for durability (none for a push of no commands); otherwise the reason
/// every command is refused with.
pub(super) type Landed = (Result<(), String>, Result<Option<u64>, String>);

/// Brings a push's pack into the odb and moves its refs, or leaves both
/// as they were.
///
/// The store lock is held from the first object to the ref transaction
/// or the rewind. A push that reads the odb therefore never finds an
/// object of a push that is refused afterwards: it could point a ref at
/// one, or leave out of its own writes one it brought too, and the rewind
/// would take the object from under that ref.
///
/// `prepared` is absent for a push refused over its signature
/// (`sig_block`), for one with no pack (`packed` false), and for a pack
/// that failed to index.
pub(super) fn land(
    writer: &WriteCoordinator,
    prepared: Option<&PreparedPack>,
    packed: bool,
    sig_block: Option<String>,
    updates: &[RefUpdate],
    principal: &Principal,
    sig_label: &str,
) -> Result<Landed, Box<dyn std::error::Error>> {
    let mut g = writer.store.lock().unwrap();
    // read from memory, no file lock: the server is the only process that
    // writes this store and the mutex keeps its other threads out. The
    // first `put` reconciles with the disk before it writes
    let before = g.odb().cursor()?;
    let unpack = match prepared {
        Some((ip, order)) => {
            apply_pack_into_store(&mut g, ip, order).map_err(|e| format!("index-pack: {e}"))
        }
        None if packed && sig_block.is_none() => Err("index-pack: failed to prepare pack".into()),
        None => Ok(Vec::new()),
    };
    // a failed ingest may have appended objects before it returned: end
    // that batch before another thread starts a durability barrier
    let unpack = match g.odb_mut().flush_pending() {
        Ok(()) => unpack,
        Err(e) => Err(format!("index-pack: {e}")),
    };
    let moved = match (&unpack, sig_block) {
        (_, Some(reason)) => Err(reason),
        (Err(_), None) => Err("pack unpack failed".into()),
        (Ok(new_commits), None) => {
            let block = match require_signed_commits_block(&g, principal, new_commits)? {
                Some(reason) => Some(reason),
                None => absent_tip_block(&g, updates)?,
            };
            match block {
                Some(reason) => Err(reason),
                None => commit_ref_updates(&mut g, &writer.group, updates, principal, sig_label),
            }
        }
    };
    // refs that moved make the objects part of the history served, even
    // if the wait for durability fails later
    if moved.is_err()
        && let Err(e) = g.odb_mut().rewind(before)
    {
        eprintln!("altd-server: rewind after rejected push failed: {e}");
    }
    Ok((unpack.map(drop), moved))
}
