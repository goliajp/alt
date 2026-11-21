//! What reading a log does about a record that does not read.
//!
//! A writer that dies mid-append leaves an unfinished record at the end of
//! its log, and damage leaves an unreadable record anywhere. Nothing in the
//! record tells the two apart: a damaged length field makes a record in the
//! middle of a pack look like the last one. What tells them apart is what
//! is known of how far the log was synced (see [`Basis`]). Damage is
//! reported with the file left as it is.
//!
//! An unfinished record is cut off only by an opener that holds the lock
//! writers of the log append under ([`Tail::locked`]), and by a handle
//! catching up under that lock. Every other open passes over it: the file
//! stays as it is and the handle holds the log up to that record.

use std::sync::Arc;

/// What is known of how far a log holds whole records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// records were synced whole up to this length: a record that does not
    /// read before it is damage, one at or past it never finished
    Boundary(u64),
    /// nothing is recorded, as in a store an older program wrote: only a
    /// record no whole record can follow is taken for an unfinished one
    LastOnly,
    /// what was recorded is gone: every record that does not read is damage
    Unknown,
}

type Judge = Arc<dyn Fn(u32, u64) -> Basis + Send + Sync>;
type BeforeCut = Arc<dyn Fn(u32, u64) -> std::io::Result<()> + Send + Sync>;

/// How a log's unreadable end is told from damage, and whether the file may
/// be repaired.
#[derive(Clone)]
pub struct Tail {
    basis: Option<Judge>,
    before_cut: Option<BeforeCut>,
    /// a pack and a length it is known to have been synced whole up to
    synced: Option<(u32, u64)>,
    read_only: bool,
    locked: bool,
    deferred: bool,
    /// the run over records left unsynced that the opener may look in
    boot_run: Option<crate::runs::BootRun>,
}

/// A store nothing is known about and that no one else writes: its opener
/// may repair it, and takes every record that does not read for damage.
impl Default for Tail {
    fn default() -> Self {
        Self {
            basis: None,
            before_cut: None,
            synced: None,
            read_only: false,
            locked: true,
            deferred: false,
            boot_run: None,
        }
    }
}

impl Tail {
    /// `basis` says what is known of a log — pack `seq` of a chunk store,
    /// whatever `seq` for a record log — that is `len` bytes long. It is
    /// asked only when a record does not read. The opener is taken not to
    /// hold the writers' lock (see [`Self::locked`]).
    pub fn judged_by(basis: impl Fn(u32, u64) -> Basis + Send + Sync + 'static) -> Self {
        Self {
            basis: Some(Arc::new(basis)),
            before_cut: None,
            synced: None,
            read_only: false,
            locked: false,
            deferred: false,
            boot_run: None,
        }
    }

    /// `before_cut` is called before a log is cut back, with the pack (as
    /// for [`Self::judged_by`]) and the length the log is cut to. Whoever
    /// keeps the lengths the log was fsynced at brings them down there:
    /// what grows back past the cut was not covered by that fsync.
    pub fn before_cut(
        mut self,
        before_cut: impl Fn(u32, u64) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.before_cut = Some(Arc::new(before_cut));
        self
    }

    /// Pack `seq` is known synced whole up to `len`: while it is no longer
    /// than that, what is known of it is not asked for.
    pub fn synced_to(mut self, seq: u32, len: u64) -> Self {
        self.synced = Some((seq, len));
        self
    }

    /// The same policy for an open that must leave every file as it is:
    /// nothing is created, rebuilt or cut.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// The same policy for an opener that holds the lock writers append
    /// under: no append is under way, so an unfinished record is cut off.
    pub fn locked(mut self) -> Self {
        self.locked = true;
        self
    }

    /// The same policy for a store whose opener cuts unfinished records off
    /// itself, once it has seen what names them: they are passed over
    /// under the writers' lock too, and the handle says so
    /// ([`crate::reclog::RecLog::unfinished`]). Nothing may be appended
    /// before they are cut.
    pub fn deferring(mut self) -> Self {
        self.deferred = true;
        self
    }

    /// The same policy for an opener of a record log that may look
    /// records up in the boot run `named` instead of reading them.
    pub fn with_boot_run(mut self, named: Option<crate::runs::BootRun>) -> Self {
        self.boot_run = named;
        self
    }

    pub(crate) fn boot_run(&self) -> Option<&crate::runs::BootRun> {
        self.boot_run.as_ref()
    }

    /// No longer takes the pack [`Self::synced_to`] named for synced; says
    /// whether one was named.
    pub(crate) fn forget_synced(&mut self) -> bool {
        self.synced.take().is_some()
    }

    pub(crate) fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Whether an unfinished record is cut off the file rather than passed
    /// over.
    pub(crate) fn cuts(&self) -> bool {
        self.repairs() && !self.deferred
    }

    /// Whether a file cut short inside its header gets the header written.
    pub(crate) fn repairs(&self) -> bool {
        self.locked && !self.read_only
    }

    /// Whether unfinished records are left for the opener to cut.
    pub(crate) fn defers(&self) -> bool {
        self.deferred
    }

    /// Announces that the log (pack `seq` of a chunk store) is about to be
    /// cut back to `at`.
    pub(crate) fn cut(&self, seq: u32, at: u64) -> std::io::Result<()> {
        match &self.before_cut {
            Some(before_cut) => before_cut(seq, at),
            None => Ok(()),
        }
    }

    /// Whether pack `seq`, `len` bytes long, is known without asking
    /// [`Self::basis`] to have been synced to its end.
    pub(crate) fn synced_past(&self, seq: u32, len: u64) -> bool {
        self.synced
            .is_some_and(|(pack, upto)| pack == seq && len <= upto)
    }

    /// What is known of the log (pack `seq` of a chunk store), `len` bytes
    /// long.
    pub(crate) fn basis(&self, seq: u32, len: u64) -> Basis {
        self.basis
            .as_ref()
            .map_or(Basis::Unknown, |basis| basis(seq, len))
    }

    /// Whether a record that does not read at `at`, in a log `len` bytes
    /// long, is an unfinished write. `last`: by the format alone no whole
    /// record can follow it.
    pub(crate) fn unfinished(&self, seq: u32, len: u64, at: u64, last: bool) -> bool {
        match self.basis(seq, len) {
            Basis::Boundary(synced) => at >= synced,
            Basis::LastOnly => last,
            Basis::Unknown => false,
        }
    }
}
