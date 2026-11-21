//! Fixed-size, checksummed record logs (`blobmap`, `tier1`, `map.alt`): an
//! append-only file that is the truth, plus index runs ([`crate::runs`]) so
//! opening reads only the records appended since the last run.
//!
//! Each record ends in an 8-byte checksum over the rest of it. Records
//! behind the runs are verified when a lookup reads them; the tail past the
//! runs is verified on open, where a record that does not read is an
//! unfinished append or is reported as corruption (see [`Tail`]).

mod cache;
mod check;
mod removal;
mod sync;
mod tail;
#[cfg(test)]
mod tests;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use crate::pack::read_exact_at;
use crate::runs::{GUARD_SPAN, Guard, Key, Runs};
use crate::{StoreError, Tail};
use alt_hash::IdMap;
pub use removal::Removal;
use tail::End;

const HEADER_LEN: usize = 5;
const MAX_REC: usize = 96;
/// Marks a run position as belonging to a record's secondary key.
const SECONDARY: u64 = 1 << 63;
/// Tail length, in records, from which a flush folds the tail into the runs.
pub const INDEX_AT: usize = 256;
const SCAN_BYTES: usize = 1 << 20;

/// What distinguishes one record log from another.
#[derive(Clone, Copy)]
pub struct Layout {
    pub magic: [u8; 4],
    pub version: u8,
    pub rec_len: usize,
    pub bad_header: &'static str,
    pub bad_version: &'static str,
    pub corrupt: &'static str,
    pub bad_rewind: &'static str,
    /// a record's lookup key and, where the log has one, its second key
    pub keys: fn(&[u8]) -> (Key, Option<Key>),
}

impl Layout {
    fn header(&self) -> [u8; HEADER_LEN] {
        let m = self.magic;
        [m[0], m[1], m[2], m[3], self.version]
    }

    /// Checks the header of a log file and returns its length. A file a
    /// crash left shorter than its header counts as holding just that, and
    /// gets the header written when `policy` cuts.
    fn checked_len(&self, file: &mut File, policy: &Tail) -> Result<u64, StoreError> {
        let header = self.header();
        let size = file.metadata()?.len();
        let mut start = [0u8; HEADER_LEN];
        let have = (size as usize).min(HEADER_LEN);
        read_exact_at(file, &mut start[..have], 0)?;
        if have == HEADER_LEN {
            return if start[..4] != self.magic {
                Err(StoreError::Format(self.bad_header))
            } else if start[4] != self.version {
                Err(StoreError::Format(self.bad_version))
            } else {
                Ok(size)
            };
        }
        // anything that is not a header prefix is foreign
        if !header.starts_with(&start[..have]) {
            return Err(StoreError::Format(self.bad_header));
        }
        if policy.repairs() {
            policy.cut(0, HEADER_LEN as u64)?;
            file.set_len(0)?;
            file.write_all(&header)?;
            file.sync_all()?;
        }
        Ok(HEADER_LEN as u64)
    }

    fn intact(&self, rec: &[u8]) -> bool {
        let (body, check) = rec.split_at(self.rec_len - 8);
        alt_hash::blake3::hash(body).as_bytes()[..8] == *check
    }
}

/// The trailing checksum for a record body.
pub fn checksum(body: &[u8]) -> [u8; 8] {
    alt_hash::blake3::hash(body).as_bytes()[..8]
        .try_into()
        .unwrap()
}

/// One record, copied out of the log.
#[derive(Clone, Copy)]
pub struct Record {
    buf: [u8; MAX_REC],
    len: usize,
}

impl Deref for Record {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

pub struct RecLog {
    layout: Layout,
    file: File,
    idx_dir: PathBuf,
    runs: Runs,
    /// records in `[runs.covered(), len)`, verified
    tail: Vec<u8>,
    tail_keys: IdMap<Key, u32>,
    tail_second: IdMap<Key, Vec<u32>>,
    cache: cache::ReadCache,
    len: u64,
    /// an append that never finished follows `len` in the file
    unfinished: bool,
    unsynced: bool,
    policy: Tail,
}

/// Opens the log for append, creating it with its header when missing; the
/// second value says whether it was created. Append mode: every write lands
/// at the end of the file, wherever a truncation moved it.
fn open_for_append(path: &Path, header: &[u8]) -> Result<(File, bool), StoreError> {
    match OpenOptions::new().read(true).append(true).open(path) {
        Ok(file) => Ok((file, false)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .append(true)
                .open(path)?;
            file.write_all(header)?;
            file.sync_all()?;
            Ok((file, true))
        }
        Err(e) => Err(e.into()),
    }
}

fn guard_at(file: &File, layout: &Layout, size: u64, end: u64) -> Option<Guard> {
    let first = HEADER_LEN as u64 + layout.rec_len as u64;
    if end < first || end > size || !(end - HEADER_LEN as u64).is_multiple_of(layout.rec_len as u64)
    {
        return None;
    }
    let span = end.min(GUARD_SPAN as u64);
    let mut guard = vec![0u8; span as usize];
    read_exact_at(file, &mut guard, end - span).ok()?;
    Some(guard)
}

impl RecLog {
    /// Opens the log at `path`, creating it when missing; the second value
    /// says whether it was created. Index runs live in `<path>.idx/`.
    pub fn open(path: &Path, layout: Layout) -> Result<(Self, bool), StoreError> {
        Self::open_with_runs(path, layout, None, Tail::default())
    }

    /// The same, opening the index runs `named` (see [`Runs::open_named`])
    /// instead of reading the index directory, and treating a record that
    /// does not read as `policy` says. A read-only open creates nothing: a
    /// log that is not there is an error.
    pub fn open_with_runs(
        path: &Path,
        layout: Layout,
        named: Option<&[(u64, u64)]>,
        policy: Tail,
    ) -> Result<(Self, bool), StoreError> {
        assert!(layout.rec_len <= MAX_REC && layout.rec_len > 8);
        let mut idx_dir = path.as_os_str().to_owned();
        idx_dir.push(".idx");
        let idx_dir = PathBuf::from(idx_dir);
        let header = layout.header();

        let (mut file, created) = if policy.is_read_only() {
            (File::open(path)?, false)
        } else {
            open_for_append(path, &header)?
        };
        let size = layout.checked_len(&mut file, &policy)?;

        let guard = |end| guard_at(&file, &layout, size, end);
        let runs = match named {
            Some(named) => Runs::open_named(&idx_dir, HEADER_LEN as u64, named, guard)?,
            None => Runs::open(&idx_dir, HEADER_LEN as u64, guard)?,
        };
        let mut this = Self {
            layout,
            file,
            idx_dir,
            runs,
            tail: Vec::new(),
            tail_keys: IdMap::default(),
            tail_second: IdMap::default(),
            cache: cache::ReadCache::default(),
            len: size,
            unfinished: false,
            unsynced: false,
            policy,
        };
        let cut = this.policy.cuts();
        this.load_tail(size, End::MaybeTorn { cut })?;
        this.unfinished = !cut && this.len < size;
        Ok((this, created))
    }

    fn push_tail(&mut self, records: &[u8]) {
        let rec_len = self.layout.rec_len;
        let first = (self.tail.len() / rec_len) as u32;
        self.tail.extend_from_slice(records);
        self.tail_keys.reserve(records.len() / rec_len);
        for (at, rec) in (first..).zip(records.chunks_exact(rec_len)) {
            let (key, second) = (self.layout.keys)(rec);
            self.tail_keys.insert(key, at);
            if let Some(second) = second {
                self.tail_second.entry(second).or_default().push(at);
            }
        }
    }

    fn tail_record(&self, at: u32) -> &[u8] {
        let rec_len = self.layout.rec_len;
        &self.tail[at as usize * rec_len..(at as usize + 1) * rec_len]
    }

    fn copy(&self, rec: &[u8]) -> Record {
        let mut buf = [0u8; MAX_REC];
        buf[..rec.len()].copy_from_slice(rec);
        Record {
            buf,
            len: rec.len(),
        }
    }

    /// Reads and verifies the indexed record at `position`.
    fn read(&self, position: u64) -> Result<Record, StoreError> {
        let mut buf = [0u8; MAX_REC];
        let len = self.layout.rec_len;
        read_exact_at(&self.file, &mut buf[..len], position)?;
        if !self.layout.intact(&buf[..len]) {
            return Err(StoreError::Format(self.layout.corrupt));
        }
        Ok(Record { buf, len })
    }

    /// The latest record whose key is `key`.
    pub fn get(&self, key: &Key) -> Result<Option<Record>, StoreError> {
        if let Some(&at) = self.tail_keys.get(key) {
            return Ok(Some(self.copy(self.tail_record(at))));
        }
        if let Some(rec) = self.cache.get(key, self.layout.rec_len) {
            return Ok(Some(rec));
        }
        let mut last = None;
        self.runs.find_each(key, |position| {
            if position & SECONDARY == 0 {
                last = Some(position);
            }
        })?;
        let Some(position) = last else {
            return Ok(None);
        };
        let rec = self.read(position)?;
        if (self.layout.keys)(&rec).0 != *key {
            return Err(StoreError::Format(self.layout.corrupt));
        }
        self.cache.put(*key, &rec);
        Ok(Some(rec))
    }

    pub fn contains(&self, key: &Key) -> Result<bool, StoreError> {
        Ok(self.get(key)?.is_some())
    }

    /// Every record whose second key is `key`, in append order.
    pub fn get_second(&self, key: &Key) -> Result<Vec<Record>, StoreError> {
        let mut positions = Vec::new();
        self.runs.find_each(key, |position| {
            if position & SECONDARY != 0 {
                positions.push(position & !SECONDARY);
            }
        })?;
        let mut out = Vec::with_capacity(positions.len());
        for position in positions {
            let rec = self.read(position)?;
            if (self.layout.keys)(&rec).1 != Some(*key) {
                return Err(StoreError::Format(self.layout.corrupt));
            }
            out.push(rec);
        }
        for &at in self.tail_second.get(key).into_iter().flatten() {
            out.push(self.copy(self.tail_record(at)));
        }
        Ok(out)
    }

    /// Records whose key lies in `lo..=hi`, at most `cap` of them.
    pub fn range(&self, lo: &Key, hi: &Key, cap: usize) -> Result<Vec<Record>, StoreError> {
        let mut positions = Vec::new();
        self.runs.range(lo, hi, |_, position| {
            if position & SECONDARY == 0 {
                positions.push(position);
            }
            positions.len() < cap
        })?;
        let mut out = Vec::with_capacity(positions.len());
        for position in positions {
            out.push(self.read(position)?);
        }
        for rec in self.tail.chunks_exact(self.layout.rec_len) {
            if out.len() >= cap {
                break;
            }
            let key = (self.layout.keys)(rec).0;
            if key >= *lo && key <= *hi {
                out.push(self.copy(rec));
            }
        }
        Ok(out)
    }

    /// Visits every record in append order, verifying each.
    pub fn for_each<E: From<StoreError>>(
        &self,
        mut each: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        self.each_from(HEADER_LEN as u64, |_, rec| each(rec))
    }

    /// Whether an append that never finished follows the records held
    /// here: the file is longer than [`Self::appended_len`].
    pub fn unfinished(&self) -> bool {
        self.unfinished
    }

    /// For a handle that only reads: holds the log as ending at `at`, a
    /// record boundary. The records from there on stay in the file; a
    /// later [`Self::sync_from_disk`] reads them again. A run is written
    /// once its records are fsynced and before the marker says so, so
    /// one may reach past `at`: it stays on disk too, and this handle
    /// reads the records before `at` it covered as its tail.
    pub fn pass_over_from(&mut self, at: u64) -> Result<(), StoreError> {
        if at >= self.len {
            return Ok(());
        }
        let covered = self.runs.covered_up_to(at);
        let kept = if covered < self.runs.covered() {
            let kept = self.read_records(covered, at, End::Whole)?;
            self.runs.pass_over_past(at);
            kept
        } else {
            self.tail[..(at - covered) as usize].to_vec()
        };
        self.set_tail(&kept);
        Ok(())
    }

    /// Records in the log.
    pub fn count(&self) -> usize {
        (self.len as usize - HEADER_LEN) / self.layout.rec_len
    }

    /// Appends one record (checksum included) at the write cursor.
    pub fn append(&mut self, rec: &[u8]) -> Result<(), StoreError> {
        debug_assert_eq!(rec.len(), self.layout.rec_len);
        // a run left behind by a log that was cut back must be gone before
        // the log regrows under it
        self.runs.remove_stale()?;
        self.file.write_all(rec)?;
        self.unsynced = true;
        self.len += rec.len() as u64;
        self.push_tail(rec);
        Ok(())
    }

    pub fn sync(&mut self) -> Result<(), StoreError> {
        if !crate::relaxed_durability() {
            self.fsync()?;
        }
        Ok(())
    }

    /// Raw fsync (no relaxed gate), for the group commit layer.
    pub fn fsync(&mut self) -> Result<(), StoreError> {
        self.file.sync_all()?;
        self.unsynced = false;
        Ok(())
    }

    /// Bytes up to our write cursor.
    pub fn appended_len(&self) -> u64 {
        self.len
    }

    /// The log's true on-disk size.
    pub fn file_len(&self) -> Result<u64, StoreError> {
        Ok(self.file.metadata()?.len())
    }
}

impl Drop for RecLog {
    fn drop(&mut self) {
        // best-effort durability; explicit sync() is the checked path
        if self.unsynced && !crate::relaxed_durability() {
            let _ = self.file.sync_all();
        }
    }
}
