//! What a push brings into the server odb, and what its refs may point at.

use alt_cli::native::{Principal, Store};
use alt_git_codec::HashAlgo;
use alt_wire::push::RefUpdate;

use super::signed_commits::{Checked, added_block, check_pack};
use super::{WriteCoordinator, commit_ref_updates};

/// A pack indexed and ready to replay (file-system reads done, lock not
/// yet taken). The second tuple element is the order of `(offset, idx)`
/// pairs so the put loop visits records in file order.
pub(super) type PreparedPack = (alt_git_pack::IndexedPack, Vec<(u64, u32)>);

/// Index the pack already on disk at `path` (no store lock needed —
/// `index_pack` walks the file and builds the .idx, a CPU + disk task
/// orthogonal to in-process odb state). The returned (`IndexedPack`,
/// offset order) goes to [`land`], which takes the store lock and
/// replays the puts.
fn prepare_pack_for_ingest(
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
/// store guard.
fn apply_pack_into_store(
    store: &mut Store,
    ip: &alt_git_pack::IndexedPack,
    order: &[(u64, u32)],
) -> Result<(), Box<dyn std::error::Error>> {
    let idx = ip.idx();
    for &(offset, i) in order {
        let obj = ip.read_at(offset)?;
        store.odb_mut().put(idx.oid_at(i), obj.kind, &obj.data)?;
    }
    store.odb_mut().flush()?;
    Ok(())
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

/// A push as it stands once its commands are read and its pack is in a
/// file.
pub(super) struct Push<'a> {
    /// the pack, absent when the push sent none
    pub(super) pack: Option<&'a std::path::Path>,
    /// the reason the push is refused over its signature, if it is
    pub(super) sig_block: Option<String>,
    pub(super) updates: &'a [RefUpdate],
    pub(super) principal: &'a Principal,
    pub(super) sig_label: &'a str,
}

/// Brings a push's pack into the odb and moves its refs, or leaves both
/// as they were: whatever stops the push after its objects were written,
/// a refusal or an error, they are taken out again.
///
/// The pack is indexed and its commits are checked against the
/// `require-signed-commits` policy before the store lock is taken. The
/// lock is then held from the first object to the ref transaction or the
/// rollback. A push that reads the odb therefore never finds an object of
/// a push that is refused afterwards: it could point a ref at one, or
/// leave out of its own writes one it brought too, and the rollback would
/// take the object from under that ref.
pub(super) fn land(
    writer: &WriteCoordinator,
    mut push: Push<'_>,
) -> Result<Landed, Box<dyn std::error::Error>> {
    // a pack that fails to index is reported as the unpack error
    let mut prepared = match (push.pack, &push.sig_block) {
        (Some(path), None) => prepare_pack_for_ingest(path).map_err(|_| ()),
        _ => Err(()),
    };
    let checked = match push.sig_block {
        None => match check_pack(&writer.store, push.principal, prepared.as_ref().ok())? {
            Some(Err(reason)) => {
                (push.sig_block, prepared) = (Some(reason), Err(()));
                None
            }
            Some(Ok(checked)) => Some(checked),
            None => None,
        },
        Some(_) => None,
    };
    let mut g = writer.store.lock().unwrap();
    // where the odb ends once this handle has read what any other process
    // wrote, with the odb's write lock held from here to the flush: the
    // rollback cuts back to this and takes this push's objects only
    let begun = match &prepared {
        Ok(_) => Some(g.odb_mut().begin_batch()?),
        Err(()) => None,
    };
    let landed = settle(
        &mut g,
        &writer.group,
        &push,
        prepared.ok().as_ref(),
        checked.as_ref(),
    );
    // refs that moved make the objects part of the history served, even
    // if the wait for durability fails later
    if !matches!(landed, Ok((_, Ok(_))))
        && let Some(begun) = begun
    {
        let ended = g.odb().cursor()?;
        // nothing of this push to take out when all it brought was there
        let rewound = match ended == begun {
            true => Ok(true),
            false => g.odb_mut().rewind_batch(begun, ended),
        };
        match rewound {
            Ok(true) => {}
            Ok(false) => eprintln!(
                "altd-server: objects of a rejected push left in place (another process wrote after them)"
            ),
            Err(e) => eprintln!("altd-server: rewind after rejected push failed: {e}"),
        }
    }
    landed
}

/// The part of [`land`] between the odb's write lock being taken and the
/// rollback: the objects, the checks that need them, the ref transaction.
fn settle(
    g: &mut Store,
    group: &alt_cli::group_commit::GroupCommit,
    push: &Push<'_>,
    prepared: Option<&PreparedPack>,
    checked: Option<&Checked>,
) -> Result<Landed, Box<dyn std::error::Error>> {
    let unpack = match prepared {
        Some((ip, order)) => {
            apply_pack_into_store(g, ip, order).map_err(|e| format!("index-pack: {e}"))
        }
        None if push.pack.is_some() && push.sig_block.is_none() => {
            Err("index-pack: failed to prepare pack".into())
        }
        None => Ok(()),
    };
    // a failed ingest may have appended objects before it returned: end
    // that batch before another thread starts a durability barrier
    let unpack = match g.odb_mut().flush_pending() {
        Ok(()) => unpack,
        Err(e) => Err(format!("index-pack: {e}")),
    };
    let moved = match (&unpack, &push.sig_block) {
        (_, Some(reason)) => Err(reason.clone()),
        (Err(_), None) => Err("pack unpack failed".into()),
        (Ok(()), None) => {
            let block = match absent_tip_block(g, push.updates)? {
                Some(reason) => Some(reason),
                None => match checked {
                    Some(checked) => added_block(g, checked, push.updates)?,
                    None => None,
                },
            };
            match block {
                Some(reason) => Err(reason),
                None => commit_ref_updates(g, group, push.updates, push.principal, push.sig_label),
            }
        }
    };
    Ok((unpack, moved))
}
