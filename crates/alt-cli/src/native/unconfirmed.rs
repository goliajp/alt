//! Operations a power cut left in the log without the objects they name.
//!
//! A ref transaction is appended once its objects are written, and synced
//! after they are, but until then the kernel may write the transaction
//! out and not the objects. After a power cut such a transaction starts
//! past the boundary the log's marker records: no command was told it
//! happened. A command that writes cuts the log from the first one on
//! before it does anything else. A command that only reads and fails on
//! such a repository runs again on the log as it stood before them, and
//! says so; it changes no file.

use alt_oplog::Op;

use super::*;
use crate::daemon::Response;

/// Whether every object `op` makes a ref point at is stored.
fn held(op: &Op, stored: &dyn Fn(&ObjectId) -> Res<bool>) -> Res<bool> {
    for oid in alt_refs::named_objects(op)? {
        if !stored(&oid)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn discarded(ops: usize) -> String {
    format!(
        "warning: discarded {ops} operation(s) at the end of the operation log: they were never \
         synced to disk and name objects that are not stored\n"
    )
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
    /// stored ([`Self::settle`]).
    pub fn open_for(alt_dir: PathBuf, writing: bool) -> Res<Self> {
        let mut store = Self::open(alt_dir)?;
        if writing && !store.refs.confirmed() {
            store.settle()?;
        }
        Ok(store)
    }

    /// [`Self::refresh`] for a command that writes: the operation log is
    /// also cut from the first operation on that was never synced and
    /// makes a ref point at an object which is not stored. The object
    /// store is kept from being appended to meanwhile, so that a live
    /// writer's objects are all seen.
    pub fn settle(&mut self) -> Res<()> {
        self.refresh()?;
        if self.refs.confirmed() {
            return Ok(());
        }
        let refs = &mut self.refs;
        let cut = self
            .odb
            .settled(|stored| refs.settle(|op| held(op, &|oid| Ok(stored(oid)?))))?;
        if let Some(cut) = cut? {
            crate::diagnostics::emit(discarded(cut.ops).as_bytes())?;
        }
        Ok(())
    }

    /// For a command that only reads: how many operations at the end of
    /// the log a command that writes would cut, and where the first of
    /// them starts.
    pub(crate) fn unconfirmed(&self) -> Res<Option<alt_refs::Unconfirmed>> {
        if self.refs.confirmed() {
            return Ok(None);
        }
        // nothing has settled the object store: an object is stored when
        // it reads
        self.refs
            .unconfirmed(|op| held(op, &|oid| Ok(matches!(self.odb.get(oid), Ok(Some(_))))))
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

/// The answer to a command: `first` unless the command only reads, failed,
/// and the repository holds operations that were never synced and name
/// objects which are not stored. `attempt` then runs it again, in this
/// process, on the log as it stood before those operations.
pub fn answer_before_unconfirmed(first: Response, attempt: impl Fn() -> Response) -> Response {
    if first.exit_code == 0 {
        return first;
    }
    let found = (|| -> Res<Option<alt_refs::Unconfirmed>> {
        let Ok(parsed) = crate::machine::parse(std::env::args_os().collect()) else {
            return Ok(None);
        };
        let command = &parsed.cli.command;
        // the check reports the repository as it is
        if crate::cli::mutates_command(command)
            || matches!(command, crate::cli::Command::Fsck { .. })
        {
            return Ok(None);
        }
        let cwd = std::env::current_dir()?;
        let (alt_dir, _) = resolve_workspace(&cwd, parsed.cli.workspace.as_deref())?;
        let found = Store::open_read_only(alt_dir.clone())?.unconfirmed()?;
        if let Some(found) = found {
            alt_oplog::end_reads_at(&alt_dir.join("oplog"), found.at)?;
        }
        Ok(found)
    })();
    let Ok(Some(found)) = found else {
        return first;
    };
    crate::client::stay_direct();
    let mut again = attempt();
    if again.exit_code == 0 {
        let warning = format!(
            "warning: the last {} operation(s) in the operation log were never synced to disk \
             and name objects that are not stored; this shows the repository as it was before \
             them. The next command that writes discards them.\n",
            found.ops
        );
        again.stderr.splice(0..0, warning.into_bytes());
    }
    again
}
