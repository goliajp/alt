//! What a push brings into the server odb, and what its refs may point at.
//!
//! The objects of a push are written only once nothing can refuse it any
//! more. Its pack is indexed where it was received, and every check reads
//! the objects from there and from the odb: that the new values exist,
//! that the commits the push adds satisfy `require-signed-commits`, that
//! the refs are where the push expects them. Those checks run as the
//! state check of the ref transaction, under the log's lock and against
//! the refs as they are then, the operations of other processes read in;
//! the objects are written from there, and the transaction is appended
//! behind them. A push that is refused has written nothing, so no other
//! process can have come to depend on an object of it.
//!
//! `odb.lock` is taken before all of that and held until the refs moved,
//! or until the objects are taken out again where appending the
//! transaction failed after they were written: no other process appends
//! in between, so the rollback takes this push's objects and no others,
//! and none has left an object out of its own writes because this push
//! had brought it. Where that rollback cannot be done (the write filled
//! the active pack, which is sealed then), the objects stay with no ref
//! at them: a ref comes to point at one only through a push that passes
//! the checks above, as for any object the odb holds unreferenced.

use std::cell::RefCell;
use std::collections::BTreeMap;

use alt_cli::native::Principal;
use alt_git_codec::{HashAlgo, ObjectId, RawObject};
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefTarget};
use alt_wire::push::RefUpdate;

use super::WriteCoordinator;
use super::ref_tx::commit_ref_updates;
use super::signed_commits::{Checked, check_pack};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// A pack indexed and ready to replay (file-system reads done, lock not
/// yet taken). The second tuple element is the order of `(offset, idx)`
/// pairs so the put loop visits records in file order.
pub(super) type PreparedPack = (alt_git_pack::IndexedPack, Vec<(u64, u32)>);

/// Index the pack already on disk at `path` (no store lock needed —
/// `index_pack` walks the file and builds the .idx, a CPU + disk task
/// orthogonal to in-process odb state).
fn prepare_pack_for_ingest(path: &std::path::Path) -> Res<PreparedPack> {
    let indexed = alt_git_pack::index_pack(path, HashAlgo::Sha1, true)?;
    let ip = alt_git_pack::IndexedPack::open(&indexed.pack_path, HashAlgo::Sha1)?;
    let idx = ip.idx();
    let mut order: Vec<(u64, u32)> = (0..idx.len())
        .map(|i| (idx.offset_at(i).expect("idx in range"), i))
        .collect();
    order.sort_unstable();
    Ok((ip, order))
}

/// The objects a push may name: those the odb holds and those of its
/// pack, which is not in the odb yet.
pub(super) struct Objects<'a> {
    odb: &'a NativeOdb,
    pack: Option<&'a PreparedPack>,
}

impl Objects<'_> {
    pub(super) fn get(&self, oid: &ObjectId) -> Res<Option<RawObject>> {
        if let Some(held) = self.odb.get(oid)? {
            return Ok(Some(held));
        }
        match self.pack {
            Some((ip, _)) => Ok(ip.read(oid)?.map(|packed| packed.to_raw())),
            None => Ok(None),
        }
    }

    fn contains(&self, oid: &ObjectId) -> Res<bool> {
        let packed = |(ip, _): &PreparedPack| ip.idx().lookup(oid).is_some();
        Ok(self.pack.is_some_and(packed) || self.odb.contains(oid)?)
    }
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

/// What stopped a push in the state check of its ref transaction.
enum Stopped {
    Refused(String),
    /// writing the pack failed
    Unpack(String),
    Failed(Box<dyn std::error::Error>),
}

/// A push from `odb.lock` being taken to its ref transaction.
struct Landing<'a> {
    odb: &'a mut NativeOdb,
    pack: Option<&'a PreparedPack>,
    checked: Option<&'a Checked>,
    updates: &'a [RefUpdate],
    /// whether objects of the pack went into the odb
    wrote: bool,
    stopped: Option<Stopped>,
}

impl Landing<'_> {
    /// The reason to refuse the push with the refs at `refs`: a new value
    /// that neither the odb nor the pack holds, or a commit the push adds
    /// that fails the policy.
    fn refusal(&self, refs: &BTreeMap<String, RefTarget>) -> Res<Option<String>> {
        let objects = Objects {
            odb: self.odb,
            pack: self.pack,
        };
        let new: Vec<ObjectId> = self.updates.iter().filter_map(|u| u.new).collect();
        for oid in &new {
            if !objects.contains(oid)? {
                return Ok(Some("missing necessary objects".into()));
            }
        }
        let Some(checked) = self.checked else {
            return Ok(None);
        };
        let served: Vec<ObjectId> = refs
            .values()
            .filter_map(|target| match target {
                RefTarget::Oid(oid) => Some(*oid),
                RefTarget::Symbolic(_) => None,
            })
            .collect();
        checked.added_refusal(&objects, &served, &new)
    }

    fn write(&mut self) -> Res<()> {
        let Some((ip, order)) = self.pack else {
            return Ok(());
        };
        self.wrote = true;
        for &(offset, i) in order {
            let obj = ip.read_at(offset)?;
            self.odb.put(ip.idx().oid_at(i), obj.kind, &obj.data)?;
        }
        Ok(())
    }

    /// The state check of the ref transaction: the objects are written
    /// when nothing refuses the push.
    fn vet(
        &mut self,
        refs: &BTreeMap<String, RefTarget>,
        changes: &[RefChange],
    ) -> Result<(), String> {
        // the transaction refuses this as a conflict right after
        if changes.iter().any(|c| refs.get(&c.name) != c.old.as_ref()) {
            return Ok(());
        }
        let stopped = match self.refusal(refs) {
            Ok(None) => match self.write() {
                Ok(()) => return Ok(()),
                Err(e) => Stopped::Unpack(format!("index-pack: {e}")),
            },
            Ok(Some(reason)) => Stopped::Refused(reason),
            Err(e) => Stopped::Failed(e),
        };
        self.stopped = Some(stopped);
        Err("push stopped".into())
    }
}

/// Brings a push's pack into the odb and moves its refs, or leaves both
/// as they were (see the module's description). The pack is indexed and
/// the signatures of its commits are checked before the store lock is
/// taken.
pub(super) fn land(writer: &WriteCoordinator, push: Push<'_>) -> Res<Landed> {
    if let Some(reason) = &push.sig_block {
        return Ok((Ok(()), Err(reason.clone())));
    }
    // a pack that fails to index is reported as the unpack error
    let prepared = match push.pack.map(prepare_pack_for_ingest) {
        Some(Err(_)) => {
            let unpack = Err("index-pack: failed to prepare pack".into());
            return Ok((unpack, Err("pack unpack failed".into())));
        }
        Some(Ok(prepared)) => Some(prepared),
        None => None,
    };
    // nothing would name the objects
    if push.updates.is_empty() {
        return Ok((Ok(()), Ok(None)));
    }
    let checked = check_pack(&writer.store, push.principal, prepared.as_ref())?;
    transact(writer, &push, prepared.as_ref(), checked.as_ref())
}

/// The part of [`land`] under the store lock. The ticket to wait on for
/// durability is handed out once the batch of objects has ended: a sink
/// syncs the odb under the shared `odb.lock`, and so never between this
/// push's objects and its ref transaction.
fn transact(
    writer: &WriteCoordinator,
    push: &Push<'_>,
    pack: Option<&PreparedPack>,
    checked: Option<&Checked>,
) -> Res<Landed> {
    let mut g = writer.store.lock().unwrap();
    let caps = g.capabilities_for(push.principal);
    let (refs, odb) = g.refs_and_odb();
    // where the odb ends once this handle has read what any other process
    // wrote: a rollback cuts back to this
    let begun = odb.begin_batch()?;
    let landing = RefCell::new(Landing {
        odb,
        pack,
        checked,
        updates: push.updates,
        wrote: false,
        stopped: None,
    });
    let vet = |refs: &BTreeMap<String, RefTarget>, changes: &[RefChange]| {
        landing.borrow_mut().vet(refs, changes)
    };
    let committed = commit_ref_updates(refs, &caps, push, &vet);
    let landing = landing.into_inner();
    let odb = landing.odb;
    let failed = match committed {
        // the refs moved: the objects are part of the history served,
        // even if making them durable fails
        Ok(()) => {
            let moved = match odb.flush() {
                Ok(()) => Ok(Some(writer.group.assign())),
                Err(e) => Err(format!("durability: {e}")),
            };
            return Ok((Ok(()), moved));
        }
        Err(e) => e,
    };
    if landing.wrote
        && let Err(e) = odb.rewind(begun)
    {
        eprintln!("altd-server: objects of a push that failed left in place: {e}");
    }
    if let Err(e) = odb.flush_pending() {
        eprintln!("altd-server: ending the batch of a push that failed: {e}");
    }
    match landing.stopped {
        Some(Stopped::Failed(e)) => Err(e),
        Some(Stopped::Unpack(e)) => Ok((Err(e), Err("pack unpack failed".into()))),
        Some(Stopped::Refused(reason)) => Ok((Ok(()), Err(reason))),
        None => Ok((Ok(()), Err(format!("ref tx: {failed}")))),
    }
}
