//! Operations a power cut left in the log without the objects they name.
//!
//! A ref transaction is appended once its objects are written, and synced
//! after they are, but until then the kernel may write the transaction
//! out and not the objects. After a power cut such a transaction starts
//! past the boundary the log's marker records: no command was told it
//! happened. A command that writes cuts the log from the first one on
//! before it does anything else. A command that only reads answers from
//! the log as it stood before them, and says so; it changes no file.
//!
//! # The order locks are taken in
//!
//! Settling is where the locks of the object store and of the operation
//! log are held together. A thread that waits for a lock holds none that
//! comes after it here:
//!
//! 1. `requests/<id>/lock`, for as long as a command that writes runs
//! 2. the mutex a daemon keeps its store behind
//! 3. `write.lock` of the workspace a command runs in, then that of one
//!    it adds
//! 4. `task-data/lock`
//! 5. `odb.sync.lock`, around the fsync of the object store
//! 6. `odb.lock`: exclusive for a batch of appends, shared for an open,
//!    for that fsync and for a sink's
//! 7. `oplog/log`, for an append, a cut or a read of the log
//! 8. `oplog/sync.lock`, around the fsync of the log and a cut of it
//! 9. a marker, `odb.durable` or `oplog/durable`, locked on itself for
//!    one update with nothing else taken meanwhile
//!
//! So nothing that runs with `oplog/log` locked touches the object store
//! unless it took `odb.lock` first, as settling does, and a batch of
//! appends may be open when an op is appended. A lock that is only ever
//! asked for without waiting (another request's, a migration journal's)
//! has no place in the order.

use alt_oplog::Op;

use super::*;
use crate::daemon::Response;

mod discarded;
mod staged;
pub use discarded::{AfterDiscards, Discards};

/// Whether every object `op` names is stored: those a ref transaction
/// makes refs point at, and the blobs of an index change (`also`).
fn held(op: &Op, also: &alt_refs::AlsoNamed, stored: &dyn Fn(&ObjectId) -> Res<bool>) -> Res<bool> {
    for oid in alt_refs::named_objects(op)?.into_iter().chain(also(op)) {
        if !stored(&oid)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The ref store of a repository whose objects are in the object store
/// beside it: before it records as synced an op another writer appended,
/// the objects stored by then are synced, that writer's among them.
pub(super) fn open_refs(alt_dir: &Path) -> Res<RefStore> {
    let mut refs = RefStore::open(alt_dir)?;
    let alt_dir = alt_dir.to_owned();
    refs.sync_objects_with(std::sync::Arc::new(move || {
        NativeOdb::sync_stored(&alt_dir).map_err(std::io::Error::other)
    }));
    Ok(refs)
}

impl Store {
    /// Opens the store for a command; one that writes starts from a log
    /// without unsynced operations that name objects which are not
    /// stored ([`Self::settle`]), one that only reads changes nothing
    /// ([`Self::open_reading`]).
    pub fn open_for(alt_dir: PathBuf, writing: bool) -> Res<Self> {
        if !writing {
            return Self::open_reading(alt_dir);
        }
        let mut store = Self::open(alt_dir)?;
        if store.refs.unsettled() {
            store.settle()?;
        }
        Ok(store)
    }

    /// [`Self::refresh`] for a command that writes: the operation log is
    /// also cut from the first operation on that was never synced and
    /// makes a ref point at an object which is not stored. The object
    /// store is kept from being appended to meanwhile, so that a live
    /// writer's objects are all seen. Where the log's marker records no
    /// boundary, or every operation lies before it, there is nothing to
    /// look at and no lock is taken.
    ///
    /// What is cut is told by whoever runs the command
    /// ([`Self::warn_discarded`]), or with the failure when this fails.
    pub fn settle(&mut self) -> Res<()> {
        let settled = (|| -> Res<()> {
            self.refresh()?;
            if !self.refs.unsettled() {
                return Ok(());
            }
            let refs = &mut self.refs;
            let also = prepared_blobs(&self.alt_dir);
            let cut = self
                .odb
                .settled(|stored| refs.settle(|op| held(op, &also, &|oid| Ok(stored(oid)?))))?;
            cut.map(drop)
        })();
        settled.map_err(|error| self.after_discards(error))
    }

    /// The ref store and the object store together, for a ref transaction
    /// whose state check reads or writes objects: its caller takes
    /// `odb.lock` before the transaction takes the log's.
    pub fn refs_and_odb(&mut self) -> (&mut RefStore, &mut NativeOdb) {
        (&mut self.refs, &mut self.odb)
    }

    /// For a command that only reads: how many operations at the end of
    /// the log a command that writes would cut, and where the first of
    /// them starts.
    pub(crate) fn unconfirmed(&self) -> Res<Option<alt_refs::Unconfirmed>> {
        if self.refs.confirmed() {
            return Ok(None);
        }
        // nothing has settled the object store: an object is stored when
        // it reads and its record is not one a writer cuts off
        let gone = self.odb.unconfirmed_objects()?;
        let stored =
            |oid: &ObjectId| !gone.contains(oid) && matches!(self.odb.get(oid), Ok(Some(_)));
        let also = prepared_blobs(&self.alt_dir);
        self.refs
            .unconfirmed(|op| held(op, &also, &|oid| Ok(stored(oid))))
    }
}

impl Store {
    /// The refs that point at an object which does not read, as the refs
    /// were before the last `dropped` operations of the log.
    pub(crate) fn dangling_refs(&self, dropped: usize) -> Res<Vec<(String, ObjectId)>> {
        let mut dangling = Vec::new();
        for (name, target) in self.refs.refs_before(dropped)? {
            if let RefTarget::Oid(oid) = target
                && !matches!(self.odb.get(&oid), Ok(Some(_)))
            {
                dangling.push((name, oid));
            }
        }
        Ok(dangling)
    }
}

/// The answer to a command, told when it shows the repository without
/// the last operations of the log: a command that only reads leaves out
/// those that were never synced and name objects which are not stored or
/// follow a record that does not read.
pub fn tell_left_out(mut response: Response) -> Response {
    if let (0, Some((named, after))) = (response.exit_code, alt_refs::left_out()) {
        let why = match (named, after) {
            (_, 0) => "and name objects that are not stored".to_owned(),
            (0, _) => "and follow a record that does not read".to_owned(),
            _ => format!(
                "({after} of them follow a record that does not read, the others name objects \
                 that are not stored)"
            ),
        };
        let ops = named + after;
        let warning = format!(
            "warning: the last {ops} operation(s) in the operation log were never synced to disk \
             {why}; this shows the repository as it was before them. The next command that \
             writes discards them.\n"
        );
        response.stderr.splice(0..0, warning.into_bytes());
    }
    response
}
