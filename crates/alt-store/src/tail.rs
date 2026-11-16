//! What reading a log does about a record that does not read.
//!
//! A writer that dies mid-append leaves an unfinished record at the end of
//! its log, and damage leaves an unreadable record anywhere. Nothing in the
//! record tells the two apart: a damaged length field makes a record in the
//! middle of a pack look like the last one. What tells them apart is how
//! far the log was synced: an unreadable record that starts before that
//! point was once whole and is damage, reported with the file left as it
//! is; one that starts at or past it is cut off. Without a synced length
//! nothing is cut.

use std::sync::Arc;

type SyncedLen = Arc<dyn Fn(u32) -> Option<u64> + Send + Sync>;
type BeforeCut = Arc<dyn Fn(u32, u64) -> std::io::Result<()> + Send + Sync>;

/// How a log's unreadable end is told from damage, and whether the file may
/// be repaired at all.
#[derive(Clone, Default)]
pub struct Tail {
    synced: Option<SyncedLen>,
    before_cut: Option<BeforeCut>,
    read_only: bool,
}

impl Tail {
    /// `synced` says how far the log is known to hold whole, synced
    /// records — of pack `seq` for a chunk store, whatever `seq` for a
    /// record log — or `None` when that is not known. It is asked only
    /// when a record does not read.
    pub fn synced_to(synced: impl Fn(u32) -> Option<u64> + Send + Sync + 'static) -> Self {
        Self {
            synced: Some(Arc::new(synced)),
            before_cut: None,
            read_only: false,
        }
    }

    /// `before_cut` is called before a log is cut back, with the pack (as
    /// for [`Self::synced_to`]) and the length the log is cut to. Whoever
    /// keeps the lengths the log was fsynced at brings them down there:
    /// what grows back past the cut was not covered by that fsync.
    pub fn before_cut(
        mut self,
        before_cut: impl Fn(u32, u64) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.before_cut = Some(Arc::new(before_cut));
        self
    }

    /// The same policy for an open that must leave every file as it is: an
    /// unfinished record is passed over instead of cut off, and nothing is
    /// created or rebuilt.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    pub(crate) fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Announces that the log (pack `seq` of a chunk store) is about to be
    /// cut back to `at`.
    pub(crate) fn cut(&self, seq: u32, at: u64) -> std::io::Result<()> {
        match &self.before_cut {
            Some(before_cut) => before_cut(seq, at),
            None => Ok(()),
        }
    }

    /// Whether a record that does not read at `at` is an unfinished write.
    pub(crate) fn unfinished(&self, seq: u32, at: u64) -> bool {
        self.synced
            .as_ref()
            .and_then(|synced| synced(seq))
            .is_some_and(|synced| at >= synced)
    }
}
