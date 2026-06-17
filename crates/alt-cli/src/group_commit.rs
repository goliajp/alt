//! In-process group commit coordinator.
//!
//! Each commit calls [`GroupCommit::assign`] right after appending (under
//! the store `Mutex`) to receive a monotonic ticket, then calls
//! [`GroupCommit::await_durable`] to block until its ticket is durable.
//! At any moment at most one thread is the "leader" performing the
//! fsync; the rest wait on a condition variable. The leader's fsync
//! covers every ticket `<= next_ticket` at the moment it samples it, so
//! N concurrent commits coalesce onto ~1 fsync — the lever that lifts
//! throughput past the one-fsync-per-commit ceiling.
//!
//! The fsync runs **without** the store `Mutex` — the caller hands in a
//! handle ([`crate::native::StoreSink`]) that owns its own fds, so
//! concurrent appends overlap the flush.
//!
//! M5 daemon path used a local copy of this code; M14/W44 lifts it
//! into a shared module so `altd-server` can group-coalesce its
//! receive-pack fsyncs against multi-client traffic by the same logic.

use std::sync::{Condvar, Mutex};

use crate::native::StoreSink;

pub struct GroupCommit {
    inner: Mutex<GroupInner>,
    cv: Condvar,
}

struct GroupInner {
    /// Highest ticket handed out (each commit takes `next_ticket += 1`).
    next_ticket: u64,
    /// Highest ticket made durable by a completed fsync.
    durable: u64,
    /// A leader is currently fsyncing; others wait rather than pile on.
    syncing: bool,
    /// Error from the most recent fsync (clears on the next success),
    /// so a committer whose own flush failed reports it instead of
    /// looping a dead disk.
    last_error: Option<String>,
    /// Total fsyncs performed (success + failure both increment). M14/W44
    /// exposes this so a test can prove coalescing actually happened —
    /// `fsync_count < commit_count` under N-way concurrency means the
    /// group commit did its job.
    fsync_count: u64,
}

impl Default for GroupCommit {
    fn default() -> Self {
        Self::new()
    }
}

impl GroupCommit {
    pub fn new() -> Self {
        GroupCommit {
            inner: Mutex::new(GroupInner {
                next_ticket: 0,
                durable: 0,
                syncing: false,
                last_error: None,
                fsync_count: 0,
            }),
            cv: Condvar::new(),
        }
    }

    /// Total fsyncs performed since startup. Used by tests to assert
    /// coalescing.
    pub fn fsync_count(&self) -> u64 {
        self.inner.lock().expect("group mutex poisoned").fsync_count
    }

    /// Hand out the next ticket. Caller must already hold the store
    /// mutex (so tickets are in commit order and the bytes for this
    /// ticket are on disk before the ticket is observable).
    pub fn assign(&self) -> u64 {
        let mut g = self.inner.lock().expect("group mutex poisoned");
        g.next_ticket += 1;
        g.next_ticket
    }

    /// Block until `ticket` is durable. The first caller into the
    /// uncovered region becomes the leader and performs the fsync;
    /// later callers wait on the condvar. A leader whose own flush
    /// fails surfaces the error rather than retry a dead disk.
    pub fn await_durable(&self, sink: &StoreSink, ticket: u64) -> Result<(), String> {
        let mut g = self.inner.lock().expect("group mutex poisoned");
        let mut led = false;
        loop {
            if g.durable >= ticket {
                return Ok(());
            }
            if g.syncing {
                g = self.cv.wait(g).expect("group mutex poisoned");
                continue;
            }
            if led {
                // we already fsynced once and are still uncovered → our
                // flush failed; surface it rather than spin a dead disk
                return Err(g
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "durability failed".to_owned()));
            }
            // become the leader for this batch
            g.syncing = true;
            led = true;
            let covered = g.next_ticket;
            drop(g);
            let outcome = sink
                .fsync_all()
                .map(|()| covered)
                .map_err(|e| e.to_string());
            g = self.inner.lock().expect("group mutex poisoned");
            g.syncing = false;
            g.fsync_count += 1;
            match outcome {
                Ok(covered) => {
                    g.durable = g.durable.max(covered);
                    g.last_error = None;
                }
                Err(e) => g.last_error = Some(e),
            }
            self.cv.notify_all();
        }
    }
}
