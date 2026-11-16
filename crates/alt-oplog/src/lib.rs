//! alt operation log: the transactional spine of the `.alt` state layer.
//!
//! Every state change is one op record appended to `oplog/log`. An op's id
//! is the BLAKE3 of its body, and each body names its parent's id, so the
//! log is a hash chain: replaying from the root reproduces (and verifies)
//! the exact sequence of operations. Undo is a later op that moves state
//! back, never a rewrite — the log itself only ever grows.
//!
//! Record framing (all integers little-endian):
//!
//! ```text
//! [body_len u32][body][checksum 8 = blake3(body)[..8]]
//! body = [parent 32][change_id 32, reserved][timestamp_ms u64]
//!        [actor_len u16][actor][payload_len u32][payload]
//! ```
//!
//! The checksum is the op id's own prefix, so verifying a record and
//! computing its id are one hash. A record that does not read is cut off
//! on open when it lies past where the log was last synced (an append
//! that never finished); anywhere else it is corruption and is reported,
//! never silently skipped or cut.
//!
//! Pure-logic crate, business-agnostic: payloads are opaque bytes; what an
//! op *means* (ref updates, imports, …) is the caller's encoding.

mod lock;
mod op;
mod read;
mod scan;
mod synced;

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

pub use op::{Op, OpId, OpLogError, ROOT};
pub use synced::OpLogSink;

use lock::{lock_exclusive, lock_shared, unlock};
use read::{read_at, read_log, scan, scan_and_repair, unfinished};
use scan::{CHECKSUM_LEN, HEADER_LEN, check_header, encode_body, replay_records};
use synced::Marker;

/// A place in the log to start reading from instead of its first record:
/// the op whose record occupies bytes `start..end`, being op number `count`
/// of the chain (counting from one). Whoever derives state from the log up
/// to an op (the ref snapshot) keeps one, so a later open replays only what
/// came after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub op: OpId,
    pub start: u64,
    pub end: u64,
    pub count: u64,
}

/// The append-only operation log under `<dir>/log`.
pub struct OpLog {
    file: File,
    /// Set when the log was opened at an anchor: the ops up to and including
    /// it are not in `ops`.
    base: Option<Anchor>,
    /// The ops after `base` (all of them without one), in chain order.
    ops: Vec<Op>,
    /// The whole chain, read on first use when `base` hides part of it.
    full: OnceLock<Vec<Op>>,
    /// Byte offset just past the last record we have replayed — where the
    /// next record goes, absent a concurrent writer. Lets `append` read only
    /// the tail another process may have added, rather than the whole file.
    len_bytes: u64,
    /// Byte offset where the last record starts.
    last_start: u64,
    /// Group commit: a lock around the fsync + a marker of how far the log is
    /// durable, so N concurrent committers coalesce to ~1 fsync.
    sync_lock_path: PathBuf,
    durable_path: PathBuf,
    /// Deferred durability (the daemon): when set, `sync` skips its inline
    /// fsync (the daemon's group-commit coordinator fsyncs off the write path,
    /// via [`OpLog::sync_handle`], so concurrent appends overlap the fsync).
    /// flock does not serialize threads of one process, so the cross-process
    /// marker machinery alone cannot batch them — the daemon coordinates
    /// in-process.
    defer: bool,
    /// Counts deferred writes, so the daemon can tell whether a request wrote.
    write_count: u64,
    /// Whether this handle appended anything a drop should still fsync.
    unsynced: bool,
    /// How far this handle's deferred appends reach, for its sink to record
    /// once it has fsynced.
    deferred: Arc<AtomicU64>,
}

impl OpLog {
    /// Opens (or creates) the log in `dir`, replaying and verifying the
    /// whole chain. A torn tail is truncated; a broken chain or corrupt
    /// record refuses to open.
    pub fn open(dir: &Path) -> Result<Self, OpLogError> {
        Self::open_at(dir, None)
    }

    /// Opens (or creates) the log in `dir`, replaying and verifying only the
    /// records after `anchor` when the log still holds the anchor's record
    /// where the anchor says; the whole chain otherwise. The ops before the
    /// anchor are read (and verified) on the first call that needs them.
    ///
    /// The file is read under a shared lock: writers append under the
    /// exclusive one, so a short tail seen here is a crashed writer's, never
    /// a live append in flight. Repairing it (or writing the header of a new log)
    /// retakes the lock exclusively and rescans, since another opener may
    /// have repaired it or a writer appended in between.
    pub fn open_at(dir: &Path, anchor: Option<Anchor>) -> Result<Self, OpLogError> {
        let path = dir.join("log");
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(dir)?;
                OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(&path)?
            }
            Err(e) => return Err(e.into()),
        };

        // only the read needs the lock; verifying the copy does not
        lock_shared(&file)?;
        let raw = read_log(&file, anchor);
        let _ = unlock(&file);
        let mut scan = scan(raw?)?;
        if scan.needs_repair() {
            lock_exclusive(&file)?;
            let repaired = scan_and_repair(&mut file, anchor, &dir.join("durable"));
            let _ = unlock(&file);
            scan = repaired?;
        }
        Ok(Self::opened(dir, file, scan))
    }

    /// Opens the log in `dir` to be read and nothing else: nothing is
    /// created or repaired, and an unfinished append at the end of the log
    /// is passed over.
    pub fn open_read_only(dir: &Path, anchor: Option<Anchor>) -> Result<Self, OpLogError> {
        let file = File::open(dir.join("log"))?;
        lock_shared(&file)?;
        let raw = read_log(&file, anchor);
        let _ = unlock(&file);
        let scan = scan(raw?)?.unrepaired(&dir.join("durable"))?;
        Ok(Self::opened(dir, file, scan))
    }

    fn opened(dir: &Path, file: File, scan: read::Scan) -> Self {
        Self {
            file,
            base: scan.base,
            ops: scan.ops,
            full: OnceLock::new(),
            len_bytes: scan.valid_len.max(HEADER_LEN as u64),
            last_start: scan.last_start,
            sync_lock_path: dir.join("sync.lock"),
            durable_path: dir.join("durable"),
            defer: false,
            write_count: 0,
            unsynced: false,
            deferred: Arc::default(),
        }
    }

    /// Appends one op, chaining it onto the **current on-disk head**, and
    /// returns its id. Cross-process safe: an exclusive `flock` brackets a
    /// short critical section that first catches up on any ops another writer
    /// appended (so this op parents on the true head, not a stale in-memory
    /// one), then writes. Concurrent writers serialize on the lock; the hash
    /// chain stays linear and unbroken.
    pub fn append(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<OpId, OpLogError> {
        if actor.len() > u16::MAX as usize {
            return Err(OpLogError::TooLarge(actor.len()));
        }
        if u32::try_from(payload.len()).is_err() {
            return Err(OpLogError::TooLarge(payload.len()));
        }

        lock_exclusive(&self.file)?;
        let result = self.append_locked(actor, timestamp_ms, payload);
        // release explicitly; the lock is also dropped if the fd closes
        let _ = unlock(&self.file);
        result
    }

    /// Catches up, then validates+builds the payload, then appends — the whole
    /// read-modify-write under one exclusive lock. `build` is called with the
    /// ops [`Self::recent`] holds *now* (post-catch-up) so a higher layer
    /// (e.g. the ref store) can fold concurrent ops into its own state and
    /// CAS-validate its transaction against the true current state,
    /// atomically with the append. `build` returning `Err` aborts cleanly
    /// with nothing written.
    pub fn append_checked<F, E>(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        build: F,
    ) -> Result<OpId, E>
    where
        F: FnOnce(&[Op]) -> Result<Vec<u8>, E>,
        E: From<OpLogError>,
    {
        if actor.len() > u16::MAX as usize {
            return Err(OpLogError::TooLarge(actor.len()).into());
        }
        lock_exclusive(&self.file).map_err(OpLogError::from)?;
        let result = self.append_checked_locked(actor, timestamp_ms, build);
        let _ = unlock(&self.file);
        result
    }

    /// Read-path catch-up for a long-lived log (the daemon between requests):
    /// replays records appended by other writers so reads see the current
    /// chain, writing nothing. Like `append` it takes the exclusive lock (so a
    /// torn tail from a crashed writer is truncated safely).
    pub fn refresh(&mut self) -> Result<(), OpLogError> {
        lock_exclusive(&self.file)?;
        let result = self.catch_up();
        let _ = unlock(&self.file);
        result
    }

    /// The body of `append`, run while holding the exclusive lock.
    fn append_locked(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<OpId, OpLogError> {
        self.catch_up()?;
        self.write_record(actor, timestamp_ms, payload)
    }

    /// The body of `append_checked`, run while holding the exclusive lock.
    fn append_checked_locked<F, E>(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        build: F,
    ) -> Result<OpId, E>
    where
        F: FnOnce(&[Op]) -> Result<Vec<u8>, E>,
        E: From<OpLogError>,
    {
        self.catch_up().map_err(E::from)?;
        let payload = build(&self.ops)?;
        if u32::try_from(payload.len()).is_err() {
            return Err(OpLogError::TooLarge(payload.len()).into());
        }
        self.write_record(actor, timestamp_ms, &payload)
            .map_err(E::from)
    }

    /// Encodes and appends one record onto the current head and folds it into
    /// the in-memory chain. The caller must hold the lock and have caught up.
    fn write_record(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<OpId, OpLogError> {
        let parent = self.head().unwrap_or(ROOT);
        let body = encode_body(&parent, timestamp_ms, actor, payload);
        let hash = alt_hash::blake3::hash(&body);

        let mut rec = Vec::with_capacity(4 + body.len() + CHECKSUM_LEN);
        rec.extend_from_slice(&(body.len() as u32).to_le_bytes());
        rec.extend_from_slice(&body);
        rec.extend_from_slice(&hash.as_bytes()[..CHECKSUM_LEN]);
        self.file.seek(SeekFrom::Start(self.len_bytes))?;
        self.file.write_all(&rec)?;
        self.unsynced = true;
        self.last_start = self.len_bytes;
        self.len_bytes += rec.len() as u64;

        let op = Op {
            id: OpId(*hash.as_bytes()),
            parent,
            change_id: [0u8; 32],
            timestamp_ms,
            actor: actor.to_owned(),
            payload: payload.to_vec(),
        };
        if let Some(full) = self.full.get_mut() {
            full.push(op.clone());
        }
        self.ops.push(op);
        Ok(OpId(*hash.as_bytes()))
    }

    /// Replays any records appended past our known tail (by another writer),
    /// extending the in-memory chain so the next append parents on the real
    /// head. A torn tail left by a crashed writer is truncated (we hold the
    /// lock, so no live writer is mid-append).
    fn catch_up(&mut self) -> Result<(), OpLogError> {
        let size = self.file.metadata()?.len();
        if size <= self.len_bytes {
            return Ok(());
        }
        let tail = read_at(&self.file, self.len_bytes, (size - self.len_bytes) as usize)?;

        let head = self.head().unwrap_or(ROOT);
        let known = self.ops.len();
        let replayed = replay_records(&tail, 0, head, &mut self.ops);
        let replayed = replayed.and_then(|replayed| {
            let new_len = self.len_bytes + replayed.valid as u64;
            if replayed.valid < tail.len() {
                unfinished(&self.durable_path, new_len)?;
            }
            Ok(replayed)
        });
        let replayed = match replayed {
            Ok(replayed) => replayed,
            Err(damage) => {
                // the handle stays where it was
                self.ops.truncate(known);
                return Err(damage);
            }
        };
        if let Some(full) = self.full.get_mut() {
            full.extend_from_slice(&self.ops[known..]);
        }
        if let Some(start) = replayed.last_start {
            self.last_start = self.len_bytes + start as u64;
        }
        let new_len = self.len_bytes + replayed.valid as u64;
        if replayed.valid < tail.len() {
            self.file.set_len(new_len)?; // drop a crashed writer's unfinished record
            self.file.sync_all()?;
        }
        self.len_bytes = new_len;
        Ok(())
    }

    /// The latest op — the state the log currently describes.
    pub fn head(&self) -> Option<OpId> {
        self.ops
            .last()
            .map(|op| op.id)
            .or(self.base.map(|anchor| anchor.op))
    }

    /// The anchor naming the current head, for a later [`Self::open_at`].
    pub fn anchor(&self) -> Option<Anchor> {
        let op = self.head()?;
        Some(Anchor {
            op,
            start: self.last_start,
            end: self.len_bytes,
            count: self.len() as u64,
        })
    }

    /// Whether the log was opened at an anchor, so [`Self::recent`] starts
    /// after it rather than at the first op.
    pub fn anchored(&self) -> bool {
        self.base.is_some()
    }

    /// The ops this handle replayed on open and since: those after the
    /// anchor it was opened at, or all of them without one.
    pub fn recent(&self) -> &[Op] {
        &self.ops
    }

    /// All ops in chain order — the replay basis. On a log opened at an
    /// anchor the first call reads and verifies the part before it.
    pub fn ops(&self) -> Result<&[Op], OpLogError> {
        let Some(base) = self.base else {
            return Ok(&self.ops);
        };
        if let Some(full) = self.full.get() {
            return Ok(full);
        }
        // the records before the anchor never change; no lock needed
        let data = read_at(&self.file, 0, base.end as usize)?;
        check_header(&data)?;
        let mut all = Vec::new();
        let replayed = replay_records(&data, HEADER_LEN.min(data.len()), ROOT, &mut all)?;
        if replayed.valid as u64 != base.end
            || all.len() as u64 != base.count
            || all.last().map(|op| op.id) != Some(base.op)
        {
            return Err(OpLogError::Format("oplog does not lead to its anchor"));
        }
        all.extend_from_slice(&self.ops);
        Ok(self.full.get_or_init(|| all))
    }

    pub fn get(&self, id: &OpId) -> Result<Option<&Op>, OpLogError> {
        Ok(self.ops()?.iter().find(|op| op.id == *id))
    }

    pub fn len(&self) -> usize {
        self.base.map_or(0, |anchor| anchor.count as usize) + self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes up to our write cursor.
    pub fn appended_len(&self) -> u64 {
        self.len_bytes
    }

    /// Durability point for everything appended so far. Group commit: fsync
    /// only if another committer has not already made our appends durable, so
    /// N concurrent commits coalesce to ~1 fsync. With `ALT_RELAXED_DURABILITY`
    /// the fsync is skipped entirely (appended bytes stay cross-process visible
    /// regardless).
    pub fn sync(&mut self) -> Result<(), OpLogError> {
        if relaxed_durability() {
            return Ok(());
        }
        if self.defer {
            // the daemon's group-commit coordinator fsyncs off the write path
            // (via `sync_handle`) and then records how far this append reached
            self.write_count += 1;
            self.deferred.store(self.len_bytes, Ordering::Release);
            return Ok(());
        }
        self.sync_to(self.len_bytes)
    }

    /// Turns deferred durability on or off (the daemon turns it on so its
    /// group-commit coordinator batches fsyncs in-process; the direct CLI
    /// leaves it off and fsyncs inline).
    pub fn set_defer_durability(&mut self, on: bool) {
        self.defer = on;
    }

    /// A monotonic count of deferred writes — the daemon snapshots it around a
    /// request to tell whether the command wrote.
    pub fn write_count(&self) -> u64 {
        self.write_count
    }

    /// An independent fsync handle on the (stable, append-only) `log` file,
    /// for the daemon's off-write-path fsync.
    pub fn sync_handle(&self) -> Result<OpLogSink, OpLogError> {
        Ok(OpLogSink {
            file: self.file.try_clone()?,
            appended: self.deferred.clone(),
            durable_path: self.durable_path.clone(),
        })
    }

    /// The fsync-coalescing core: fsync only if no other committer already made
    /// our target durable (the inline, direct-CLI path), and record `target`
    /// (a record boundary) as synced either way.
    fn sync_to(&mut self, target: u64) -> Result<(), OpLogError> {
        let recorded = |marker: Marker| marker.records(target);
        if Marker::read(&self.durable_path).is_some_and(recorded) {
            return Ok(());
        }
        let sync_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.sync_lock_path)?;
        lock_exclusive(&sync_lock)?;
        let result = (|| -> Result<(), OpLogError> {
            let marker = Marker::read(&self.durable_path);
            if marker.is_some_and(recorded) {
                return Ok(());
            }
            let eof = match marker {
                Some(marker) if marker.eof >= target => marker.eof,
                _ => {
                    let eof = self.file.metadata()?.len();
                    self.file.sync_all()?;
                    eof
                }
            };
            let whole = marker.and_then(|marker| marker.whole).unwrap_or(0);
            Marker::write(&self.durable_path, eof, whole.max(target))?;
            Ok(())
        })();
        let _ = unlock(&sync_lock);
        result
    }
}

/// Repro/diagnostic knob (env `ALT_RELAXED_DURABILITY`): skip per-commit
/// fsyncs. Used to open the concurrency race window for investigation.
pub fn relaxed_durability() -> bool {
    std::env::var_os("ALT_RELAXED_DURABILITY").is_some()
}

impl Drop for OpLog {
    fn drop(&mut self) {
        // best-effort durability; explicit sync() is the checked path
        if self.unsynced {
            let _ = self.file.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_body_preserves_reserved_fields_and_refuses_identity_changes() {
        let mut raw = scan::encode_body(&ROOT, 42, "source actor", b"source payload");
        raw[32..64].fill(7);
        let op = Op::from_body(&raw).unwrap();
        assert_eq!(op.change_id, [7; 32]);
        assert_eq!(op.body().unwrap(), raw);
        for end in 0..raw.len() {
            assert!(Op::from_body(&raw[..end]).is_err());
        }
        let mut trailing = raw.clone();
        trailing.push(0);
        assert!(Op::from_body(&trailing).is_err());
        let mut edited = op.clone();
        edited.actor.push('x');
        assert!(edited.body().is_err());
        edited.actor = "x".repeat(u16::MAX as usize + 1);
        assert!(edited.body().is_err());
    }

    #[test]
    fn appends_chain_and_replays() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        assert!(log.is_empty());
        assert_eq!(log.head(), None);

        let a = log.append("alice", 1000, b"op a").unwrap();
        let b = log.append("agent/7", 2000, b"op b").unwrap();
        assert_eq!(log.head(), Some(b));
        assert_eq!(log.ops().unwrap()[0].parent, ROOT);
        assert_eq!(log.ops().unwrap()[1].parent, a);
        assert_eq!(log.get(&a).unwrap().unwrap().payload, b"op a");
        assert_eq!(log.get(&b).unwrap().unwrap().actor, "agent/7");
    }

    #[test]
    fn ids_are_content_addressed_and_chain_dependent() {
        let dir1 = alt_os::temp::tempdir().unwrap();
        let dir2 = alt_os::temp::tempdir().unwrap();
        let mut log1 = OpLog::open(dir1.path()).unwrap();
        let mut log2 = OpLog::open(dir2.path()).unwrap();

        // same op in the same position: same id
        let a1 = log1.append("a", 1, b"x").unwrap();
        let a2 = log2.append("a", 1, b"x").unwrap();
        assert_eq!(a1, a2);

        // same op content after different parents: different id
        let b1 = log1.append("a", 2, b"y").unwrap();
        log2.append("other", 9, b"z").unwrap();
        let b2 = log2.append("a", 2, b"y").unwrap();
        assert_ne!(b1, b2);
    }

    #[test]
    fn empty_actor_and_payload_are_valid() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        let id = log.append("", 0, b"").unwrap();
        assert_eq!(log.get(&id).unwrap().unwrap().actor, "");
        assert_eq!(log.get(&id).unwrap().unwrap().payload, b"");
    }

    #[test]
    fn catches_up_on_a_concurrently_grown_log() {
        // two independent openers of the same log: one appends, the other must
        // see it (catch-up) and chain onto it rather than fork the chain.
        let dir = alt_os::temp::tempdir().unwrap();
        let mut a = OpLog::open(dir.path()).unwrap();
        let mut b = OpLog::open(dir.path()).unwrap();

        let a1 = a.append("a", 1, b"a1").unwrap();
        // b still thinks the log is empty; its append must catch up to a1 first
        let b1 = b.append("b", 2, b"b1").unwrap();
        assert_eq!(b.get(&b1).unwrap().unwrap().parent, a1, "b1 chains onto a1");

        // and a, in turn, catches up to b1 on its next append
        let a2 = a.append("a", 3, b"a2").unwrap();
        assert_eq!(a.get(&a2).unwrap().unwrap().parent, b1, "a2 chains onto b1");

        // a fresh reader sees a single unbroken chain of all three
        let reopened = OpLog::open(dir.path()).unwrap();
        assert_eq!(reopened.len(), 3);
        assert_eq!(reopened.head(), Some(a2));
    }

    #[test]
    fn concurrent_writers_keep_the_chain_linear_and_complete() {
        use std::sync::{Arc, Barrier};

        let dir = alt_os::temp::tempdir().unwrap();
        OpLog::open(dir.path()).unwrap(); // create the file up front

        const WRITERS: usize = 8;
        const PER_WRITER: usize = 25;
        let barrier = Arc::new(Barrier::new(WRITERS));
        let path = dir.path().to_path_buf();

        let mut handles = Vec::new();
        for w in 0..WRITERS {
            let barrier = Arc::clone(&barrier);
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                // each thread opens its OWN log handle → separate flock
                // descriptions, exactly like separate processes contending.
                let mut log = OpLog::open(&path).unwrap();
                barrier.wait();
                for i in 0..PER_WRITER {
                    let payload = format!("w{w}-{i}");
                    log.append(
                        &format!("agent/{w}"),
                        (w * 1000 + i) as u64,
                        payload.as_bytes(),
                    )
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // reopening replays + verifies the whole chain; success means it never
        // broke. Every writer's every op must be present exactly once.
        let log = OpLog::open(&path).unwrap();
        assert_eq!(
            log.len(),
            WRITERS * PER_WRITER,
            "no op lost and none duplicated"
        );
        let mut seen: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
        for op in log.ops().unwrap() {
            assert!(
                seen.insert(op.payload.clone()),
                "duplicate op {:?}",
                op.payload
            );
        }
        for w in 0..WRITERS {
            for i in 0..PER_WRITER {
                assert!(
                    seen.contains(format!("w{w}-{i}").as_bytes()),
                    "missing op w{w}-{i}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn open_waits_out_an_in_flight_append_instead_of_truncating_it() {
        // a writer mid-append holds the exclusive lock with half its record
        // on disk; an open must not mistake that for a crashed writer's torn
        // tail and cut it off
        let dir = alt_os::temp::tempdir().unwrap();
        OpLog::open(dir.path())
            .unwrap()
            .append("a", 1, b"first")
            .unwrap();
        let path = dir.path().join("log");
        let before = std::fs::read(&path).unwrap();

        // the exact bytes the next append would write, built on a copy
        let scratch = alt_os::temp::tempdir().unwrap();
        std::fs::write(scratch.path().join("log"), &before).unwrap();
        OpLog::open(scratch.path())
            .unwrap()
            .append("a", 2, b"second")
            .unwrap();
        let record = std::fs::read(scratch.path().join("log")).unwrap()[before.len()..].to_vec();

        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        lock_exclusive(&writer).unwrap();
        let (head, tail) = record.split_at(record.len() / 2);
        writer.write_all(head).unwrap();

        let log_dir = dir.path().to_path_buf();
        let opener = std::thread::spawn(move || OpLog::open(&log_dir).unwrap().len());
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!opener.is_finished(), "open did not wait for the writer");

        writer.write_all(tail).unwrap();
        unlock(&writer).unwrap();
        assert_eq!(opener.join().unwrap(), 2, "the in-flight record was cut");
    }
}
