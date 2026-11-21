//! `map.alt`: the bidirectional sha1/sha256 ↔ blake3 ↔ (kind, size) index —
//! the only bridge between the git-facing logical layer and the physical
//! store. It is truth, not cache: an object's kind (and thus its canonical
//! header) is not derivable from stored content alone, so records carry
//! their own checksum (see [`alt_store::reclog`] for how the log is opened
//! and indexed: by git oid, and by blake3 as the second key).

use std::fs::File;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_store::reclog::{self, Layout, RecLog};
use alt_store::{BlobId, StoreError};

use crate::OdbError;

mod prefix;

/// Record: algo u8 + git oid 32 (sha1 zero-padded) + blake3 32 + kind u8 +
/// size u64 + checksum 8. All integers little-endian.
const REC_LEN: usize = 82;
const CHECKED_LEN: usize = REC_LEN - 8;

const LAYOUT: Layout = Layout {
    magic: *b"ALTG",
    version: 1,
    rec_len: REC_LEN,
    bad_header: "bad map.alt header",
    bad_version: "unsupported map.alt version",
    corrupt: "map.alt record corrupt",
    bad_rewind: "map.alt rewind target off a record boundary",
    keys: |rec| {
        (
            rec[1..33].try_into().unwrap(),
            Some(rec[33..65].try_into().unwrap()),
        )
    },
};

/// `map.alt` is the odb's own file: its format errors are the odb's.
fn lift(e: StoreError) -> OdbError {
    match e {
        StoreError::Format(what) => OdbError::Format(what),
        other => other.into(),
    }
}

fn git_key(oid: &ObjectId) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..oid.as_bytes().len()].copy_from_slice(oid.as_bytes());
    key
}

fn algo_byte(algo: HashAlgo) -> u8 {
    match algo {
        HashAlgo::Sha1 => 1,
        HashAlgo::Sha256 => 2,
    }
}

fn kind_byte(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::Blob => 0,
        ObjectKind::Tree => 1,
        ObjectKind::Commit => 2,
        ObjectKind::Tag => 3,
    }
}

fn parse_kind(byte: u8) -> Result<ObjectKind, OdbError> {
    Ok(match byte {
        0 => ObjectKind::Blob,
        1 => ObjectKind::Tree,
        2 => ObjectKind::Commit,
        3 => ObjectKind::Tag,
        _ => return Err(OdbError::Format("bad object kind in map.alt")),
    })
}

/// One object's identities: its git oid, its content's blake3 address, and
/// the kind/size needed to rebuild the canonical `"<kind> <size>\0"` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapEntry {
    pub git: ObjectId,
    pub alt: BlobId,
    pub kind: ObjectKind,
    pub size: u64,
}

impl MapEntry {
    fn encode(&self) -> [u8; REC_LEN] {
        let mut rec = [0u8; REC_LEN];
        rec[0] = algo_byte(self.git.algo());
        rec[1..1 + self.git.as_bytes().len()].copy_from_slice(self.git.as_bytes());
        rec[33..65].copy_from_slice(&self.alt.0);
        rec[65] = kind_byte(self.kind);
        rec[66..74].copy_from_slice(&self.size.to_le_bytes());
        let check = reclog::checksum(&rec[..CHECKED_LEN]);
        rec[CHECKED_LEN..].copy_from_slice(&check);
        rec
    }

    fn parse(rec: &[u8]) -> Result<Self, OdbError> {
        let algo = match rec[0] {
            1 => HashAlgo::Sha1,
            2 => HashAlgo::Sha256,
            _ => return Err(OdbError::Format("bad hash algo in map.alt")),
        };
        let git = ObjectId::from_bytes(algo, &rec[1..1 + algo.raw_len()])
            .map_err(|_| OdbError::Format("bad git oid in map.alt"))?;
        let mut alt = [0u8; 32];
        alt.copy_from_slice(&rec[33..65]);
        Ok(Self {
            git,
            alt: BlobId(alt),
            kind: parse_kind(rec[65])?,
            size: u64::from_le_bytes(rec[66..74].try_into().unwrap()),
        })
    }
}

pub struct ObjectMap {
    log: RecLog,
}

impl ObjectMap {
    pub fn open(path: &Path) -> Result<Self, OdbError> {
        Ok(Self::open_with_runs(path, None, alt_store::Tail::default())?.0)
    }

    /// The same, opening the index runs `named` by name and reading a log
    /// that does not read to its end as `tail` says; the second value says
    /// whether the map was created.
    pub fn open_with_runs(
        path: &Path,
        named: Option<&[(u64, u64)]>,
        tail: alt_store::Tail,
    ) -> Result<(Self, bool), OdbError> {
        let (log, created) = RecLog::open_with_runs(path, LAYOUT, named, tail).map_err(lift)?;
        Ok((Self { log }, created))
    }

    /// See [`RecLog::keep_reads`].
    pub fn keep_reads(&mut self) {
        self.log.keep_reads();
    }

    pub fn run_chain(&self) -> Vec<(u64, u64)> {
        self.log.run_chain()
    }

    pub fn append(&mut self, entry: MapEntry) -> Result<(), OdbError> {
        self.log.append(&entry.encode()).map_err(lift)
    }

    /// Folds in records another writer appended since our last read. Run under
    /// the odb write lock. A torn tail left by a crashed writer is truncated.
    pub fn sync_from_disk(&mut self) -> Result<(), OdbError> {
        self.log.sync_from_disk().map_err(lift)
    }

    /// Indexes the records before byte `upto` that have no index run yet,
    /// once there are enough of them. Run under the odb write lock.
    pub fn index_tail(&mut self, upto: u64) -> Result<(), OdbError> {
        self.log
            .index_tail(upto, alt_store::runs::written_synced())
            .map_err(lift)
    }

    pub fn index_pending(&self, upto: u64) -> bool {
        self.log.index_pending(upto)
    }

    pub fn by_git(&self, oid: &ObjectId) -> Result<Option<MapEntry>, OdbError> {
        let Some(rec) = self.log.get(&git_key(oid)).map_err(lift)? else {
            return Ok(None);
        };
        let entry = MapEntry::parse(&rec)?;
        // a sha1 and a sha256 oid sharing their first 20 bytes are not the
        // same object
        Ok((entry.git == *oid).then_some(entry))
    }

    /// The git identities sharing one stored content, in append order.
    pub fn by_alt(&self, id: BlobId) -> Result<Vec<MapEntry>, OdbError> {
        self.log
            .get_second(&id.0)
            .map_err(lift)?
            .iter()
            .map(|rec| MapEntry::parse(rec))
            .collect()
    }

    /// Full check of the log (see [`RecLog::check`]); `each` sees every
    /// entry recorded before byte `upto`.
    pub fn check(
        &self,
        upto: u64,
        mut each: impl FnMut(MapEntry) -> Result<(), OdbError>,
    ) -> Result<u64, OdbError> {
        self.log.check_upto(upto, |rec| each(MapEntry::parse(rec)?))
    }

    /// Visits the entries from byte `from` of the log on, each with where
    /// its record begins.
    pub fn each_from(
        &self,
        from: u64,
        mut each: impl FnMut(u64, MapEntry) -> Result<(), OdbError>,
    ) -> Result<(), OdbError> {
        self.log
            .each_from(from, |at, rec| each(at, MapEntry::parse(rec)?))
    }

    /// See [`RecLog::removal`].
    pub fn removal(&self, gone: &[u64]) -> Result<reclog::Removal, StoreError> {
        self.log.removal(gone)
    }

    /// See [`RecLog::pass_over_from`].
    pub fn pass_over_from(&mut self, at: u64) -> Result<(), OdbError> {
        self.log.pass_over_from(at).map_err(lift)
    }

    /// See [`RecLog::unfinished`].
    pub fn unfinished(&self) -> bool {
        self.log.unfinished()
    }

    /// See [`RecLog::passed`].
    pub fn passed(&self) -> Result<alt_store::Passed, StoreError> {
        self.log.passed()
    }

    /// All entries in append order.
    pub fn entries(&self) -> Result<Vec<MapEntry>, OdbError> {
        let mut out = Vec::with_capacity(self.log.count());
        let mut bad = None;
        self.log
            .for_each(|rec| -> Result<(), StoreError> {
                match MapEntry::parse(rec) {
                    Ok(entry) => out.push(entry),
                    Err(e) => bad = bad.take().or(Some(e)),
                }
                Ok(())
            })
            .map_err(lift)?;
        match bad {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    pub fn len(&self) -> usize {
        self.log.count()
    }

    pub fn is_empty(&self) -> bool {
        self.log.count() == 0
    }

    pub fn sync(&mut self) -> Result<(), OdbError> {
        self.log.sync().map_err(lift)
    }

    /// Raw fsync (no relaxed gate) — called by the group commit layer.
    pub fn fsync(&mut self) -> Result<(), OdbError> {
        self.log.fsync().map_err(lift)
    }

    /// An independent fd to the (stable, never-rewritten) `map.alt` file, for
    /// the daemon's off-write-path fsync.
    pub fn sync_handle(&self) -> Result<File, OdbError> {
        self.log.sync_handle().map_err(lift)
    }

    /// Bytes we have appended (our write cursor).
    pub fn appended_len(&self) -> u64 {
        self.log.appended_len()
    }

    /// Truncates the map back to `target_len` (a previous `appended_len`),
    /// dropping every entry appended past that point. `target_len` must be at
    /// a record boundary and not below the file header. Fsyncs the truncation
    /// so it's durable on recovery.
    pub fn rewind(&mut self, target_len: u64) -> Result<(), OdbError> {
        self.check_rewind(target_len)?;
        self.log.rewind(target_len).map_err(lift)
    }

    /// Whether [`Self::rewind`] to `target_len` can go ahead (see
    /// [`RecLog::check_rewind`]).
    pub fn check_rewind(&self, target_len: u64) -> Result<(), OdbError> {
        if target_len > self.log.appended_len() {
            return Err(OdbError::Format("map.alt rewind target above cursor"));
        }
        self.log.check_rewind(target_len).map(drop).map_err(lift)
    }

    /// The map's true on-disk size.
    pub fn file_len(&self) -> Result<u64, OdbError> {
        self.log.file_len().map_err(lift)
    }
}
