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
//! computing its id are one hash. A torn tail (crash mid-append) is
//! truncated on open; a bad record anywhere else is corruption and is
//! reported, never silently skipped.
//!
//! Business-agnostic stone: payloads are opaque bytes; what an op *means*
//! (ref updates, imports, …) is the caller's encoding.

use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: [u8; 4] = *b"ALTL";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 5;
/// parent 32 + change_id 32 + timestamp 8 + actor_len 2 + payload_len 4.
const BODY_FIXED_LEN: usize = 78;
const CHECKSUM_LEN: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum OpLogError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("oplog format: {0}")]
    Format(&'static str),
    #[error("op too large: {0} bytes")]
    TooLarge(usize),
}

/// BLAKE3 of an op's body — its identity in the chain.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpId(pub [u8; 32]);

/// The parent named by the first op in a log.
pub const ROOT: OpId = OpId([0u8; 32]);

impl fmt::Display for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OpId({self})")
    }
}

/// One recorded operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub id: OpId,
    pub parent: OpId,
    /// Reserved for the change-identity model (A5); zeros until then.
    pub change_id: [u8; 32],
    pub timestamp_ms: u64,
    /// Who performed the op — human or agent identity string.
    pub actor: String,
    /// Opaque to the log; the caller's encoding of what happened.
    pub payload: Vec<u8>,
}

fn encode_body(parent: &OpId, timestamp_ms: u64, actor: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(BODY_FIXED_LEN + actor.len() + payload.len());
    body.extend_from_slice(&parent.0);
    body.extend_from_slice(&[0u8; 32]); // change_id, reserved
    body.extend_from_slice(&timestamp_ms.to_le_bytes());
    body.extend_from_slice(&(actor.len() as u16).to_le_bytes());
    body.extend_from_slice(actor.as_bytes());
    body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    body.extend_from_slice(payload);
    body
}

fn parse_body(body: &[u8]) -> Result<Op, OpLogError> {
    if body.len() < BODY_FIXED_LEN {
        return Err(OpLogError::Format("op body too short"));
    }
    let mut parent = [0u8; 32];
    parent.copy_from_slice(&body[..32]);
    let mut change_id = [0u8; 32];
    change_id.copy_from_slice(&body[32..64]);
    let timestamp_ms = u64::from_le_bytes(body[64..72].try_into().unwrap());
    let actor_len = u16::from_le_bytes(body[72..74].try_into().unwrap()) as usize;
    let rest = &body[74..];
    if rest.len() < actor_len + 4 {
        return Err(OpLogError::Format("op actor length mismatch"));
    }
    let actor = std::str::from_utf8(&rest[..actor_len])
        .map_err(|_| OpLogError::Format("op actor is not utf-8"))?
        .to_owned();
    let payload_len =
        u32::from_le_bytes(rest[actor_len..actor_len + 4].try_into().unwrap()) as usize;
    let payload = &rest[actor_len + 4..];
    if payload.len() != payload_len {
        return Err(OpLogError::Format("op payload length mismatch"));
    }
    Ok(Op {
        id: OpId(*blake3::hash(body).as_bytes()),
        parent: OpId(parent),
        change_id,
        timestamp_ms,
        actor,
        payload: payload.to_vec(),
    })
}

/// The append-only operation log under `<dir>/log`.
pub struct OpLog {
    file: File,
    ops: Vec<Op>,
    by_id: HashMap<OpId, u32>,
    /// Byte offset just past the last record we have replayed — where the
    /// next record goes, absent a concurrent writer. Lets `append` read only
    /// the tail another process may have added, rather than the whole file.
    len_bytes: u64,
}

/// Walks length-prefixed records in `data[from..]`, verifying the hash chain
/// continues from `prev_head`, pushing each op into `ops`/`by_id`. Returns the
/// offset just past the last whole, valid record (so the caller can truncate a
/// torn tail). A corrupt record that is not the final one is real corruption.
fn replay_records(
    data: &[u8],
    from: usize,
    mut prev_head: OpId,
    ops: &mut Vec<Op>,
    by_id: &mut HashMap<OpId, u32>,
) -> Result<usize, OpLogError> {
    let mut at = from;
    let mut valid = from;
    while let Some(len_bytes) = data.get(at..at + 4) {
        let body_len = u32::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
        let Some(rec) = data.get(at + 4..at + 4 + body_len + CHECKSUM_LEN) else {
            break; // torn record at EOF
        };
        let (body, check) = rec.split_at(body_len);
        let hash = blake3::hash(body);
        if hash.as_bytes()[..CHECKSUM_LEN] != *check {
            if at + 4 + body_len + CHECKSUM_LEN == data.len() {
                break; // torn final record: caller truncates
            }
            return Err(OpLogError::Format("oplog record corrupt"));
        }
        let op = parse_body(body)?;
        if op.parent != prev_head {
            return Err(OpLogError::Format("oplog chain broken"));
        }
        prev_head = op.id;
        by_id.insert(op.id, ops.len() as u32);
        ops.push(op);
        at += 4 + body_len + CHECKSUM_LEN;
        valid = at;
    }
    Ok(valid)
}

/// Takes an exclusive advisory lock on the log file for the duration of one
/// append. `flock` is per-open-file-description and auto-releases when the fd
/// closes (incl. a crash), so there are no stale lock files. Non-unix has no
/// cross-process lock yet — single-writer there (documented limitation).
#[cfg(unix)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn unlock(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn unlock(_file: &File) -> std::io::Result<()> {
    Ok(())
}

impl OpLog {
    /// Opens (or creates) the log in `dir`, replaying and verifying the
    /// whole chain. A torn tail is truncated; a broken chain or corrupt
    /// record refuses to open.
    pub fn open(dir: &Path) -> Result<Self, OpLogError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("log");

        let existing = match std::fs::read(&path) {
            Ok(data) => Some(data),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        let Some(data) = existing else {
            let mut file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)?;
            file.write_all(&file_header())?;
            file.sync_all()?;
            return Ok(Self {
                file,
                ops: Vec::new(),
                by_id: HashMap::new(),
                len_bytes: HEADER_LEN as u64,
            });
        };

        // a crash between create and the header fsync can leave a short
        // file; anything that is not a header prefix is foreign
        if data.len() < HEADER_LEN {
            if !file_header().starts_with(&data) {
                return Err(OpLogError::Format("bad oplog header"));
            }
        } else {
            if data[..4] != MAGIC {
                return Err(OpLogError::Format("bad oplog header"));
            }
            if data[4] != VERSION {
                return Err(OpLogError::Format("unsupported oplog version"));
            }
        }

        let mut ops: Vec<Op> = Vec::new();
        let mut by_id = HashMap::new();
        // a record that runs past EOF (torn length field or torn body) ends
        // the walk; the file is truncated back to the last whole record
        let start = HEADER_LEN.min(data.len());
        let valid_len = replay_records(&data, start, ROOT, &mut ops, &mut by_id)? as u64;

        let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
        let len_bytes = if valid_len < HEADER_LEN as u64 {
            file.set_len(0)?;
            file.write_all(&file_header())?;
            file.sync_all()?;
            HEADER_LEN as u64
        } else {
            if valid_len < data.len() as u64 {
                file.set_len(valid_len)?;
                file.sync_all()?;
            }
            file.seek(SeekFrom::Start(valid_len))?;
            valid_len
        };
        Ok(Self {
            file,
            ops,
            by_id,
            len_bytes,
        })
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
    /// read-modify-write under one exclusive lock. `build` is called with every
    /// op that exists *now* (post-catch-up) so a higher layer (e.g. the ref
    /// store) can fold concurrent ops into its own state and CAS-validate its
    /// transaction against the true current state, atomically with the append.
    /// `build` returning `Err` aborts cleanly with nothing written.
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
        let hash = blake3::hash(&body);

        let mut rec = Vec::with_capacity(4 + body.len() + CHECKSUM_LEN);
        rec.extend_from_slice(&(body.len() as u32).to_le_bytes());
        rec.extend_from_slice(&body);
        rec.extend_from_slice(&hash.as_bytes()[..CHECKSUM_LEN]);
        self.file.seek(SeekFrom::Start(self.len_bytes))?;
        self.file.write_all(&rec)?;
        self.len_bytes += rec.len() as u64;

        let op = Op {
            id: OpId(*hash.as_bytes()),
            parent,
            change_id: [0u8; 32],
            timestamp_ms,
            actor: actor.to_owned(),
            payload: payload.to_vec(),
        };
        self.by_id.insert(op.id, self.ops.len() as u32);
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
        let mut tail = vec![0u8; (size - self.len_bytes) as usize];
        self.file.seek(SeekFrom::Start(self.len_bytes))?;
        self.file.read_exact(&mut tail)?;

        let head = self.head().unwrap_or(ROOT);
        let valid = replay_records(&tail, 0, head, &mut self.ops, &mut self.by_id)? as u64;
        let new_len = self.len_bytes + valid;
        if valid < tail.len() as u64 {
            self.file.set_len(new_len)?; // drop a crashed writer's torn record
            self.file.sync_all()?;
        }
        self.len_bytes = new_len;
        Ok(())
    }

    /// The latest op — the state the log currently describes.
    pub fn head(&self) -> Option<OpId> {
        self.ops.last().map(|op| op.id)
    }

    /// All ops in chain order — the replay basis.
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    pub fn get(&self, id: &OpId) -> Option<&Op> {
        self.by_id.get(id).map(|&at| &self.ops[at as usize])
    }

    /// Position of an op in chain order (for replaying from a snapshot).
    pub fn index_of(&self, id: &OpId) -> Option<usize> {
        self.by_id.get(id).map(|&at| at as usize)
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Durability point for everything appended so far.
    pub fn sync(&mut self) -> Result<(), OpLogError> {
        if !relaxed_durability() {
            self.file.sync_all()?;
        }
        Ok(())
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
        let _ = self.file.sync_all();
    }
}

fn file_header() -> [u8; HEADER_LEN] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_chain_and_replays() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        assert!(log.is_empty());
        assert_eq!(log.head(), None);

        let a = log.append("alice", 1000, b"op a").unwrap();
        let b = log.append("agent/7", 2000, b"op b").unwrap();
        assert_eq!(log.head(), Some(b));
        assert_eq!(log.ops()[0].parent, ROOT);
        assert_eq!(log.ops()[1].parent, a);
        assert_eq!(log.get(&a).unwrap().payload, b"op a");
        assert_eq!(log.get(&b).unwrap().actor, "agent/7");
    }

    #[test]
    fn ids_are_content_addressed_and_chain_dependent() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        let id = log.append("", 0, b"").unwrap();
        assert_eq!(log.get(&id).unwrap().actor, "");
        assert_eq!(log.get(&id).unwrap().payload, b"");
    }

    #[test]
    fn catches_up_on_a_concurrently_grown_log() {
        // two independent openers of the same log: one appends, the other must
        // see it (catch-up) and chain onto it rather than fork the chain.
        let dir = tempfile::tempdir().unwrap();
        let mut a = OpLog::open(dir.path()).unwrap();
        let mut b = OpLog::open(dir.path()).unwrap();

        let a1 = a.append("a", 1, b"a1").unwrap();
        // b still thinks the log is empty; its append must catch up to a1 first
        let b1 = b.append("b", 2, b"b1").unwrap();
        assert_eq!(b.get(&b1).unwrap().parent, a1, "b1 chains onto a1");

        // and a, in turn, catches up to b1 on its next append
        let a2 = a.append("a", 3, b"a2").unwrap();
        assert_eq!(a.get(&a2).unwrap().parent, b1, "a2 chains onto b1");

        // a fresh reader sees a single unbroken chain of all three
        let reopened = OpLog::open(dir.path()).unwrap();
        assert_eq!(reopened.len(), 3);
        assert_eq!(reopened.head(), Some(a2));
    }

    #[test]
    fn concurrent_writers_keep_the_chain_linear_and_complete() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
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
        for op in log.ops() {
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
}
