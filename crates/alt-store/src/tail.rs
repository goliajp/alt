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

/// How a log's unreadable end is told from damage, and whether the file may
/// be repaired at all.
#[derive(Clone, Default)]
pub struct Tail {
    synced: Option<SyncedLen>,
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
            read_only: false,
        }
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

    /// Whether a record that does not read at `at` is an unfinished write.
    pub(crate) fn unfinished(&self, seq: u32, at: u64) -> bool {
        self.synced
            .as_ref()
            .and_then(|synced| synced(seq))
            .is_some_and(|synced| at >= synced)
    }
}
