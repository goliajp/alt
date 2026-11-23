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
//! A sync that fails is the last one. The kernel reports the failure of
//! an fsync once: the pages it could not write are marked clean, and the
//! next fsync of the same file answers that all is well with the data not
//! on disk. So no later sync can vouch for what the failed one covered,
//! nor for anything appended behind it in the same files: every commit
//! not yet durable is told of the failure, and so is every commit after
//! it, until the process is started again ([`GroupCommit::failed`]).
//!
//! The fsync runs **without** the store `Mutex` — the caller hands in a
//! handle ([`crate::native::StoreSink`]) that owns its own fds, so
//! completed batches can share the flush. The sink's shared odb lock keeps
//! new object appends out of the data-to-log synchronization interval.
//!
//! The daemon path used a local copy of this code; it now lives in a
//! shared module so `altd-server` can group-coalesce its
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
    /// The error of the fsync that failed: none is made after it.
    failed: Option<String>,
    /// Total fsyncs performed (success + failure both increment).
    /// Exposed so a test can prove coalescing actually happened —
    /// `fsync_count < commit_count` under N-way concurrency means the
    /// group commit did its job.
    fsync_count: u64,
}

/// The thread that syncs for a group, while it does. A sync that panics
/// leaves nothing known of what is durable, and every commit after it
/// would wait for a leader that is gone: the process ends there, as over
/// a panic with the store in hand ([`crate::held`]).
struct Leading;

impl Drop for Leading {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("the sync of a group of commits panicked: ending");
            std::process::exit(crate::held::IN_DOUBT);
        }
    }
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
                failed: None,
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

    /// The error of the sync that failed, once one has: no commit is
    /// made durable from then on, and a caller refuses a write before it
    /// appends anything.
    pub fn failed(&self) -> Option<String> {
        let g = self.inner.lock().expect("group mutex poisoned");
        g.failed.clone()
    }

    /// Block until `ticket` is durable. The first caller into the
    /// uncovered region becomes the leader and performs the fsync;
    /// later callers wait on the condvar. Once a flush has failed,
    /// every ticket it left uncovered is answered with its error.
    pub fn await_durable(&self, sink: &StoreSink, ticket: u64) -> Result<(), String> {
        let causes = |e: Box<dyn std::error::Error>| crate::machine::causes(e.as_ref());
        self.await_durable_with(ticket, || sink.fsync_all().map_err(causes))
    }

    /// [`Self::await_durable`] with the flush supplied by the caller, so the
    /// leader/follower protocol can be driven deterministically in tests.
    fn await_durable_with<E: std::fmt::Display>(
        &self,
        ticket: u64,
        mut flush: impl FnMut() -> Result<(), E>,
    ) -> Result<(), String> {
        let mut g = self.inner.lock().expect("group mutex poisoned");
        loop {
            if g.durable >= ticket {
                return Ok(());
            }
            if let Some(failed) = &g.failed {
                return Err(failed.clone());
            }
            if g.syncing {
                g = self.cv.wait(g).expect("group mutex poisoned");
                continue;
            }
            // become the leader for this batch
            g.syncing = true;
            let covered = g.next_ticket;
            drop(g);
            let leading = Leading;
            let outcome = flush().map_err(|e| e.to_string());
            drop(leading);
            g = self.inner.lock().expect("group mutex poisoned");
            g.syncing = false;
            g.fsync_count += 1;
            match outcome {
                Ok(()) => g.durable = g.durable.max(covered),
                Err(e) => g.failed = Some(e),
            }
            self.cv.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;

    /// Commits that arrive while a leader is flushing ride on the next single
    /// fsync, however many of them there are: two fsyncs in total.
    #[test]
    fn commits_arriving_during_a_flush_share_the_next_one() {
        const FOLLOWERS: u64 = 16;
        let group = Arc::new(GroupCommit::new());
        let flushes = Arc::new(AtomicU64::new(0));

        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let leader_ticket = group.assign();
        let leader = {
            let group = Arc::clone(&group);
            let flushes = Arc::clone(&flushes);
            std::thread::spawn(move || {
                group.await_durable_with(leader_ticket, || {
                    flushes.fetch_add(1, Ordering::SeqCst);
                    started_tx.send(()).expect("test channel open");
                    release_rx.recv().expect("test channel open");
                    Ok::<(), std::io::Error>(())
                })
            })
        };
        started_rx.recv().expect("leader started its flush");

        // every follower holds a ticket before the leader's flush returns
        let followers: Vec<_> = (0..FOLLOWERS)
            .map(|_| {
                let ticket = group.assign();
                let group = Arc::clone(&group);
                let flushes = Arc::clone(&flushes);
                std::thread::spawn(move || {
                    group.await_durable_with(ticket, || {
                        flushes.fetch_add(1, Ordering::SeqCst);
                        Ok::<(), std::io::Error>(())
                    })
                })
            })
            .collect();
        release_tx.send(()).expect("leader waiting");

        leader
            .join()
            .expect("leader thread")
            .expect("leader durable");
        for f in followers {
            f.join()
                .expect("follower thread")
                .expect("follower durable");
        }
        assert_eq!(flushes.load(Ordering::SeqCst), 2);
        assert_eq!(group.fsync_count(), 2);
    }

    // run in a process of its own by the test below: one that goes on
    // after the panic ends with the status of a test that passed
    #[test]
    fn a_sync_that_panics_ends_the_process() {
        const CHILD: &str = "ALT_GROUP_COMMIT_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let group = Arc::new(GroupCommit::new());
            let ticket = group.assign();
            let leader = std::thread::spawn(move || {
                group.await_durable_with(ticket, || -> Result<(), std::io::Error> {
                    panic!("in the sync");
                })
            });
            let _ = leader.join();
            return;
        }
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "group_commit::tests::a_sync_that_panics_ends_the_process",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert_eq!(
            child.status.code(),
            Some(crate::held::IN_DOUBT),
            "{child:?}"
        );
    }

    /// What Linux does with a second fsync of a file whose first one
    /// failed: it answers that all is well, with the data not on disk.
    #[test]
    fn after_a_failed_flush_no_later_one_makes_its_batch_durable() {
        let group = GroupCommit::new();
        let (first, second) = (group.assign(), group.assign());
        let failed = group.await_durable_with(first, || Err(std::io::Error::other("disk gone")));
        assert!(failed.unwrap_err().contains("disk gone"));
        let mut again = 0;
        let flush = || {
            again += 1;
            Ok::<(), std::io::Error>(())
        };
        // appended before the flush that failed: in the same unsynced bytes
        let err = group.await_durable_with(second, flush).unwrap_err();
        assert!(err.contains("disk gone"), "{err}");
        // appended after it, behind those bytes in the same files
        let third = group.assign();
        let flush = || {
            again += 1;
            Ok::<(), std::io::Error>(())
        };
        let err = group.await_durable_with(third, flush).unwrap_err();
        assert!(err.contains("disk gone"), "{err}");
        assert_eq!(again, 0);
        assert_eq!(group.fsync_count(), 1);
    }

    /// A commit that waits while the leader's flush fails is told so: it
    /// does not lead a flush of its own.
    #[test]
    fn a_commit_waiting_on_a_flush_that_fails_is_told_it_failed() {
        let group = Arc::new(GroupCommit::new());
        let flushes = Arc::new(AtomicU64::new(0));
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let leader_ticket = group.assign();
        let leader = {
            let group = Arc::clone(&group);
            std::thread::spawn(move || {
                group.await_durable_with(leader_ticket, || {
                    started_tx.send(()).expect("test channel open");
                    release_rx.recv().expect("test channel open");
                    Err(std::io::Error::other("disk gone"))
                })
            })
        };
        started_rx.recv().expect("leader started its flush");
        let ticket = group.assign();
        let follower = {
            let group = Arc::clone(&group);
            let flushes = Arc::clone(&flushes);
            std::thread::spawn(move || {
                group.await_durable_with(ticket, || {
                    flushes.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), std::io::Error>(())
                })
            })
        };
        release_tx.send(()).expect("leader waiting");
        assert!(leader.join().expect("leader thread").is_err());
        let err = follower.join().expect("follower thread").unwrap_err();
        assert!(err.contains("disk gone"), "{err}");
        assert_eq!(flushes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failed_flush_is_reported_not_retried() {
        let group = GroupCommit::new();
        let ticket = group.assign();
        let mut calls = 0;
        let err = group
            .await_durable_with(ticket, || {
                calls += 1;
                Err(std::io::Error::other("disk gone"))
            })
            .unwrap_err();
        assert!(err.contains("disk gone"), "{err}");
        assert_eq!(calls, 1);
    }
}
