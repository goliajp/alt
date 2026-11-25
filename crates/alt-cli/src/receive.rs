//! A push as a server takes it in: what it brings into the object store,
//! and what its refs may point at.
//!
//! The pack of a push is indexed where it was received, and the push is
//! looked at before anything is written, with no lock held: against the
//! refs as they are read then, everything it adds to the history served
//! has to be held by the object store or the pack, and its commits have
//! to pass `require-signed-commits` (see [`crate::added_commits`]). A push
//! that is refused there has written nothing.
//!
//! What the push adds is written then, under `odb.lock` alone, and of the
//! pack nothing else: an object no new value reaches, or one under what
//! the refs reach already, was not looked at and stays out. Git keeps
//! such objects of a pack; here one of them could be a commit the policy
//! refuses, lying in wait under history that lacks it.
//!
//! The ref transaction comes last. Under the log's lock its state check
//! only compares: every object a ref pointed at when the push was looked
//! at, a ref still points at. Then what was found added is still all the
//! push adds, and the transaction is appended. Otherwise another process
//! moved a ref away meanwhile, the objects are taken out again and the
//! push is looked at anew, [`ATTEMPTS`] times at most. So the log's lock
//! is held for a comparison and an append, whatever the size of the push,
//! and whoever reads the refs meanwhile waits for no more than that.
//!
//! Once the transaction is appended the refs moved for whoever reads the
//! log: the objects stay then, whatever fails afterwards, a panic too.
//!
//! `odb.lock` is held from the first object written until the refs moved,
//! or until the objects are taken out again: no other process appends in
//! between, so the rollback takes this push's objects and no others, and
//! none has left an object out of its own writes because this push had
//! brought it. Where that rollback cannot be done (the write filled the
//! active pack, which is sealed then), the objects stay with no ref at
//! them. They are whole and passed the checks of this push, and a ref
//! comes to point at one only through a push that is looked at the same
//! way, as for any object the store holds unreferenced.

mod batch;
mod kept;
mod ref_tx;
pub mod signed;
mod taken;

pub use taken::{Recorded, Report, busy, take_in};

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use alt_git_codec::{HashAlgo, ObjectId, RawObject};
use alt_hash::KeyedIdSet;
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefTarget};
use alt_wire::push::RefUpdate;

use crate::added_commits::{self, Lineage, Source};
use crate::native::{Principal, Store};
use batch::Batch;
use signed::Checked;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// How many times a push is looked at before it is refused because the
/// refs keep moving under it.
pub const ATTEMPTS: usize = 4;

/// A pack indexed and ready to replay. The second element is the order
/// of `(offset, idx)` pairs, so that a write visits records in file
/// order.
pub type PreparedPack = (alt_git_pack::IndexedPack, Vec<(u64, u32)>);

/// The memory a pack took to be indexed, which it keeps for as long as its
/// objects are read out of it.
pub type PackMemory = Option<alt_git_pack::Taken<'static>>;

/// Indexes the pack on disk at `path`, which may hold what `limits`
/// allows. A push is sent thin, its deltas made of objects the server
/// has: those are read from `bases` and the pack is made whole with
/// them. Nothing else of the store is needed.
pub fn prepare_pack(
    path: &std::path::Path,
    limits: &alt_git_pack::IndexLimits<'_>,
    bases: alt_git_pack::Bases<'_>,
) -> Res<(PreparedPack, PackMemory)> {
    let indexed = alt_git_pack::index_pack_thin(path, HashAlgo::Sha1, true, limits, Some(bases))?;
    let ip = alt_git_pack::IndexedPack::open(&indexed.pack_path, HashAlgo::Sha1)?;
    let idx = ip.idx();
    let mut order: Vec<(u64, u32)> = (0..idx.len())
        .map(|i| (idx.offset_at(i).expect("idx in range"), i))
        .collect();
    order.sort_unstable();
    Ok(((ip, order), indexed.memory))
}

/// The objects a push may name: those the odb holds and those of its
/// pack, which is not in the odb yet.
pub(crate) struct Objects<'a> {
    odb: &'a NativeOdb,
    pack: Option<&'a PreparedPack>,
}

impl Objects<'_> {
    pub(crate) fn get(&self, oid: &ObjectId) -> Res<Option<RawObject>> {
        Ok(match (self.odb.get(oid)?, self.pack) {
            (Some(held), _) => Some(held),
            (None, Some((ip, _))) => ip.read(oid)?.map(|packed| packed.to_raw()),
            (None, None) => None,
        })
    }

    /// The commit `oid`; `None` for what is not held here, or is no
    /// commit.
    pub(crate) fn commit(&self, oid: &ObjectId) -> Res<Option<Arc<Lineage>>> {
        // one read before is not taken out of the store again: that it
        // is held here is not known from another repository having it
        if let Some(kept) = kept::commit(oid)
            && self.contains(oid)?
        {
            return Ok(Some(kept));
        }
        let commit = Lineage::of(oid, self.get(oid)?.as_ref());
        Ok(commit.inspect(|commit| kept::keep(oid, commit)))
    }

    fn contains(&self, oid: &ObjectId) -> Res<bool> {
        let packed = |(ip, _): &PreparedPack| ip.idx().lookup(oid).is_some();
        Ok(self.pack.is_some_and(packed) || self.odb.contains(oid)?)
    }
}

/// A push as it stands once its commands are read and its pack is
/// indexed.
pub struct Push<'a> {
    /// the pack, absent when the push sent none
    pub pack: Option<&'a PreparedPack>,
    /// what `require-signed-commits` is checked with, when the pusher is
    /// held to it
    pub checked: Option<&'a Checked>,
    pub updates: &'a [RefUpdate],
    pub principal: &'a Principal,
    /// Whom the request signed in as, where a signature names another: the
    /// push is held to what both may do. A signature tells who pushes, and
    /// takes nothing off what the account it came in by is held to.
    pub signed_in: Option<&'a Principal>,
    pub sig_label: &'a str,
    /// set once nobody waits for the push: its objects are written no further
    pub gone: Option<&'a std::sync::atomic::AtomicBool>,
}

/// What reading the objects of a push ends in once nobody waits for it.
pub(crate) fn gone_on(gone: Option<&std::sync::atomic::AtomicBool>) -> Res<()> {
    match gone.is_some_and(|gone| gone.load(std::sync::atomic::Ordering::Relaxed)) {
        true => Err("the peer of the push is gone".into()),
        false => Ok(()),
    }
}

/// What became of a push.
#[derive(Debug, PartialEq, Eq)]
pub enum Landed {
    /// the refs moved
    Moved,
    /// writing the pack failed; nothing of the push is left
    Unpack(String),
    /// every command is refused with this; nothing of the push is left
    Refused(String),
    /// the ref transaction is in the log, where whoever reads the refs
    /// sees them moved, and the push could not be made durable for this
    Unsynced(String),
}

/// Where a push is on its way in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// its objects are in the odb, once for each look at the push
    Written,
    /// its ref transaction is in the log
    Recorded,
}

fn oid(target: &RefTarget) -> Option<&ObjectId> {
    match target {
        RefTarget::Oid(oid) => Some(oid),
        RefTarget::Symbolic(_) => None,
    }
}

/// What the push adds with the refs at `served`, or why it is refused.
fn vet(
    odb: &NativeOdb,
    push: &Push<'_>,
    served: &[ObjectId],
) -> Res<Result<KeyedIdSet<ObjectId>, String>> {
    let objects = Objects {
        odb,
        pack: push.pack,
    };
    let new: Vec<ObjectId> = push.updates.iter().filter_map(|u| u.new).collect();
    let get = |oid: &ObjectId| objects.get(oid);
    let commit = |oid: &ObjectId| objects.commit(oid);
    let holds = |oid: &ObjectId| objects.contains(oid);
    let had = |oid: &ObjectId| -> Res<bool> { Ok(odb.contains(oid)?) };
    let fails = |oid: &ObjectId| match push.checked {
        Some(checked) => checked.fails(&objects, oid),
        None => Ok(None),
    };
    let source = Source {
        get: &get,
        commit: &commit,
        holds: &holds,
        had: &had,
        fails: &fails,
    };
    added_commits::added(&source, served, &new)
}

/// One look at the push and, when nothing refuses it, its objects and
/// its ref transaction; `None` when a ref moved away meanwhile.
fn attempt(
    store: &mut Store,
    push: &Push<'_>,
    tx: &ref_tx::Tx,
    observe: &dyn Fn(Step),
) -> Res<Option<Landed>> {
    let (refs, odb) = store.refs_and_odb();
    // refs before objects: a writer stores its objects before the ref
    // that names them
    refs.refresh()?;
    odb.refresh()?;
    let served: Vec<ObjectId> = refs.iter().filter_map(|(_, to)| oid(to).copied()).collect();
    // the transaction of a push that does not find the refs where it
    // expects them is refused: nothing of it needs looking at
    let expected = |u: &RefUpdate| refs.get(&u.name) == u.old.map(RefTarget::Oid).as_ref();
    let added = match push.updates.iter().all(expected) {
        true => match vet(odb, push, &served)? {
            Ok(added) => Some(added),
            Err(reason) => return Ok(Some(Landed::Refused(reason))),
        },
        false => None,
    };
    let mut batch = Batch::begin(odb)?;
    if let Some(added) = &added
        && let Err(e) = batch.write(push.pack, added, push.gone)
    {
        return Ok(Some(Landed::Unpack(format!("index-pack: {e}"))));
    }
    observe(Step::Written);
    let moved = Cell::new(false);
    let state = |now: &BTreeMap<String, RefTarget>, changes: &[RefChange]| {
        // the transaction refuses this as a conflict right after
        if changes.iter().any(|c| now.get(&c.name) != c.old.as_ref()) {
            return Ok(());
        }
        let mut pointed: KeyedIdSet<&ObjectId> = KeyedIdSet::default();
        pointed.reserve(now.len());
        pointed.extend(now.values().filter_map(oid));
        if added.is_some() && served.iter().all(|oid| pointed.contains(oid)) {
            return Ok(());
        }
        moved.set(true);
        Err("a ref moved".to_owned())
    };
    let op = match tx.append(refs, &state) {
        Ok(op) => op,
        Err(_) if moved.get() => return Ok(None),
        Err(e) => return Ok(Some(Landed::Refused(format!("ref tx: {e}")))),
    };
    // in the log, whatever fails from here on: the objects stay, synced first
    batch.keep();
    observe(Step::Recorded);
    let stored = batch.end().map_err(|e| e.to_string());
    let finished = stored.and_then(|()| tx.finish(refs, op).map_err(|e| e.to_string()));
    Ok(Some(match finished {
        Ok(()) => Landed::Moved,
        Err(e) => Landed::Unsynced(e),
    }))
}

/// Brings what a push adds into the odb and moves its refs, or leaves
/// both as they were (see the module's description).
pub fn land(store: &mut Store, push: &Push<'_>) -> Res<Landed> {
    land_observed(store, push, &|_| ())
}

/// [`land`], with `observe` called at each [`Step`] it comes to.
pub fn land_observed(store: &mut Store, push: &Push<'_>, observe: &dyn Fn(Step)) -> Res<Landed> {
    let held_to = [Some(push.principal), push.signed_in];
    let caps = held_to.into_iter().flatten();
    let caps = caps.map(|principal| store.capabilities_for(principal));
    let tx = ref_tx::Tx::new(caps.collect(), push);
    if let Err(denied) = tx.names_allowed() {
        return Ok(Landed::Refused(format!("ref tx: {denied}")));
    }
    for _ in 0..ATTEMPTS {
        if let Some(landed) = attempt(store, push, &tx, observe)? {
            return Ok(landed);
        }
    }
    Ok(Landed::Refused(
        "the refs moved while the push was looked at: push again".into(),
    ))
}
