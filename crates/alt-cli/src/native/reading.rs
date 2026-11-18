//! The store as a command that only reads holds it.
//!
//! Such a command changes no log of the repository, whatever state a
//! writer left them in: it cuts nothing off them, writes neither marker,
//! nothing into `odb.lock` or `oplog/sync.lock`, and creates no file that
//! is missing. What a writer left for the next one — an append that never
//! finished, records that were never synced, a store without record
//! boundaries — it passes over in memory, by the rules the next command
//! that writes cuts by. What it may write are caches: the ref snapshot,
//! and index runs over records that were synced and have none.

use alt_repo::Repository;

use super::*;
use crate::cli::{self, Command};

/// Whether `command` leaves the repository's store as it finds it. An
/// export writes, but elsewhere.
pub fn only_reads(command: &Command) -> bool {
    matches!(command, Command::Export { .. }) || !cli::mutates_command(command)
}

/// `repo`, of which nothing is opened yet, as `command` is to hold it.
pub fn view_for(repo: Repository, command: &Command) -> Repository {
    match only_reads(command) {
        true => repo.reading(),
        false => repo,
    }
}

/// The store under `alt_dir` and the git-layer view of it, opened the way
/// `command` is to hold them; `root` is the working tree.
pub fn open_for_command(
    command: &Command,
    alt_dir: PathBuf,
    root: &Path,
) -> Res<(Store, Repository)> {
    let repo = Repository::open_alt(alt_dir.clone(), Some(root.to_owned()))?;
    let store = match command {
        Command::Fsck { .. } => Store::open_read_only(alt_dir)?,
        _ => Store::open_for(alt_dir, cli::mutates_command(command))?,
    };
    Ok((store, view_for(repo, command)))
}

impl Store {
    fn opened(alt_dir: PathBuf, odb: NativeOdb, refs: RefStore) -> Res<Self> {
        Ok(Self {
            odb,
            refs,
            policy: Policy::load(&alt_dir)?,
            algo: std::sync::OnceLock::new(),
            alt_dir,
        })
    }

    /// Opens the store to be read and nothing else (`alt fsck`): no file
    /// under `alt_dir` is created, repaired or rebuilt.
    pub fn open_read_only(alt_dir: PathBuf) -> Res<Self> {
        let (odb, refs) = (
            NativeOdb::read_only(&alt_dir),
            RefStore::open_read_only(&alt_dir)?,
        );
        Self::opened(alt_dir, odb, refs)
    }

    /// Opens the store for a command that only reads: as
    /// [`Self::open_read_only`], but a ref snapshot that fell behind is
    /// written anew, and index runs over synced records that have none.
    /// The refs are as the log had them before the operations the next
    /// command that writes cuts (see [`RefStore::open_confirmed`]).
    pub fn open_reading(alt_dir: PathBuf) -> Res<Self> {
        let odb = NativeOdb::reading(&alt_dir);
        // an object that does not read is reported by whoever reads it
        let refs = RefStore::open_confirmed(&alt_dir, |oid| odb.contains(oid).unwrap_or(true))?;
        Self::opened(alt_dir, odb, refs)
    }

    /// For a store held across commands, before one that only reads:
    /// whether the log ends in operations that were never synced and
    /// name objects which are not stored. Such a command is answered by
    /// a process that opens the store without them.
    pub fn holds_unconfirmed(&self) -> Res<bool> {
        let stored = |oid: &ObjectId| self.odb.contains(oid).unwrap_or(true);
        Ok(self.refs.unheld(stored)?.is_some())
    }

    /// The git-layer view of this store, held the way the store is: of a
    /// store that is only read it changes no file either.
    pub(super) fn git_view(&self) -> Res<Repository> {
        let repo = Repository::discover(&self.alt_dir)?;
        Ok(match self.odb.is_read_only() {
            true => repo.reading(),
            false => repo,
        })
    }

    /// [`Self::refresh`] before a command that only reads, for a store
    /// held across commands: what other processes wrote is taken in, and
    /// no lock that a writer waits for is taken, nothing cut, nothing
    /// written.
    pub fn follow(&mut self) -> Res<()> {
        // refs before objects, as in a refresh
        self.refs.follow()?;
        self.odb.follow()?;
        self.policy = Policy::load(&self.alt_dir)?;
        Ok(())
    }
}
