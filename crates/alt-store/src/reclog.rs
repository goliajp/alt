//! Fixed-size, checksummed record logs (`blobmap`, `tier1`, `map.alt`): an
//! append-only file that is the truth, plus index runs ([`crate::runs`]) so
//! opening reads only the records appended since the last run.
//!
//! Each record ends in an 8-byte checksum over the rest of it. Records
//! behind the runs are verified when a lookup reads them; the tail past the
//! runs is verified on open, where a torn final record (crash mid-append)
//! is cut off and any other bad record is reported as corruption.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use crate::StoreError;
use crate::pack::read_exact_at;
use crate::runs::{GUARD_LEN, Guard, Key, Runs};

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
    tail_keys: HashMap<Key, u32>,
    tail_second: HashMap<Key, Vec<u32>>,
    len: u64,
    unsynced: bool,
}

fn guard_at(file: &File, layout: &Layout, size: u64, end: u64) -> Option<Guard> {
    let first = HEADER_LEN as u64 + layout.rec_len as u64;
    if end < first || end > size || !(end - HEADER_LEN as u64).is_multiple_of(layout.rec_len as u64)
    {
        return None;
    }
    let mut guard = [0u8; GUARD_LEN];
    read_exact_at(file, &mut guard, end - GUARD_LEN as u64).ok()?;
    Some(guard)
}

impl RecLog {
    /// Opens the log at `path`, creating it when missing; the second value
    /// says whether it was created. Index runs live in `<path>.idx/`.
    pub fn open(path: &Path, layout: Layout) -> Result<(Self, bool), StoreError> {
        assert!(layout.rec_len <= MAX_REC && layout.rec_len >= GUARD_LEN);
        let mut idx_dir = path.as_os_str().to_owned();
        idx_dir.push(".idx");
        let idx_dir = PathBuf::from(idx_dir);
        let header = layout.header();

        // append mode: every write lands at the end of the file, wherever a
        // truncation moved it, so the cursor is never positioned
        let (mut file, created) = match OpenOptions::new().read(true).append(true).open(path) {
            Ok(file) => (file, false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .append(true)
                    .open(path)?;
                file.write_all(&header)?;
                file.sync_all()?;
                (file, true)
            }
            Err(e) => return Err(e.into()),
        };
        let mut size = file.metadata()?.len();
        let mut start = [0u8; HEADER_LEN];
        let have = (size as usize).min(HEADER_LEN);
        read_exact_at(&file, &mut start[..have], 0)?;
        if have < HEADER_LEN {
            // a crash between create and the header fsync can leave a short
            // file; anything that is not a header prefix is foreign
            if !header.starts_with(&start[..have]) {
                return Err(StoreError::Format(layout.bad_header));
            }
            file.set_len(0)?;
            file.write_all(&header)?;
            file.sync_all()?;
            size = HEADER_LEN as u64;
        } else if start[..4] != layout.magic {
            return Err(StoreError::Format(layout.bad_header));
        } else if start[4] != layout.version {
            return Err(StoreError::Format(layout.bad_version));
        }

        let runs = Runs::open(&idx_dir, HEADER_LEN as u64, |end| {
            guard_at(&file, &layout, size, end)
        })?;
        let mut this = Self {
            layout,
            file,
            idx_dir,
            runs,
            tail: Vec::new(),
            tail_keys: HashMap::new(),
            tail_second: HashMap::new(),
            len: size,
            unsynced: false,
        };
        this.load_tail(size, true)?;
        Ok((this, created))
    }

    /// Reads and verifies `[covered, size)` as the tail. A torn end is cut
    /// off; with `strict`, a bad record before the end is corruption.
    fn load_tail(&mut self, size: u64, strict: bool) -> Result<(), StoreError> {
        let covered = self.runs.covered();
        let rec_len = self.layout.rec_len;
        let mut data = vec![0u8; (size - covered) as usize];
        read_exact_at(&self.file, &mut data, covered)?;
        let mut at = 0usize;
        while let Some(rec) = data.get(at..at + rec_len) {
            if !self.layout.intact(rec) {
                if strict && at + rec_len != data.len() {
                    return Err(StoreError::Format(self.layout.corrupt));
                }
                break;
            }
            at += rec_len;
        }
        let valid = covered + at as u64;
        if valid < size {
            self.file.set_len(valid)?;
            self.file.sync_all()?;
        }
        data.truncate(at);
        self.tail.clear();
        self.tail_keys.clear();
        self.tail_second.clear();
        self.len = valid;
        self.push_tail(&data);
        Ok(())
    }

    fn push_tail(&mut self, records: &[u8]) {
        let rec_len = self.layout.rec_len;
        let first = (self.tail.len() / rec_len) as u32;
        self.tail.extend_from_slice(records);
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
        let rec_len = self.layout.rec_len;
        let covered = self.runs.covered();
        let mut buf = vec![0u8; SCAN_BYTES / rec_len * rec_len];
        let mut at = HEADER_LEN as u64;
        while at < covered {
            let take = ((covered - at) as usize).min(buf.len());
            read_exact_at(&self.file, &mut buf[..take], at).map_err(StoreError::from)?;
            for rec in buf[..take].chunks_exact(rec_len) {
                if !self.layout.intact(rec) {
                    return Err(StoreError::Format(self.layout.corrupt).into());
                }
                each(rec)?;
            }
            at += take as u64;
        }
        for rec in self.tail.chunks_exact(rec_len) {
            each(rec)?;
        }
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

    /// Folds in records another writer appended since our last read. Run
    /// under the write lock: a torn tail there is a crashed writer's.
    pub fn sync_from_disk(&mut self) -> Result<(), StoreError> {
        self.runs.remove_stale()?;
        let size = self.file.metadata()?.len();
        if size <= self.len {
            return Ok(());
        }
        let rec_len = self.layout.rec_len;
        let mut data = vec![0u8; (size - self.len) as usize];
        read_exact_at(&self.file, &mut data, self.len)?;
        let mut at = 0usize;
        while let Some(rec) = data.get(at..at + rec_len) {
            if !self.layout.intact(rec) {
                break;
            }
            at += rec_len;
        }
        let new_len = self.len + at as u64;
        if at < data.len() {
            self.file.set_len(new_len)?;
            self.file.sync_all()?;
        }
        self.push_tail(&data[..at]);
        self.len = new_len;
        Ok(())
    }

    /// Whether the tail is long enough for [`Self::index_tail`] to act.
    pub fn index_pending(&self) -> bool {
        self.tail.len() / self.layout.rec_len >= INDEX_AT
    }

    /// Moves a long enough tail into the index runs. Run under the write
    /// lock, after `sync_from_disk`, so the log holds nothing we have not
    /// seen. `durable` fsyncs the new run before it replaces the old ones.
    pub fn index_tail(&mut self, durable: bool) -> Result<(), StoreError> {
        let rec_len = self.layout.rec_len;
        if !self.index_pending() {
            return Ok(());
        }
        // another process may have indexed part of our tail already
        let (file, layout, size) = (&self.file, &self.layout, self.len);
        let mut runs = Runs::open(&self.idx_dir, HEADER_LEN as u64, |end| {
            guard_at(file, layout, size, end)
        })?;
        let covered = runs.covered();
        let tail_start = self.len - self.tail.len() as u64;
        let skip = covered.saturating_sub(tail_start) as usize;
        let rest = &self.tail[skip.min(self.tail.len())..];
        if rest.len() / rec_len >= INDEX_AT {
            let mut entries = Vec::with_capacity(rest.len() / rec_len);
            let mut position = self.len - rest.len() as u64;
            for rec in rest.chunks_exact(rec_len) {
                let (key, second) = (self.layout.keys)(rec);
                entries.push((key, position));
                if let Some(second) = second {
                    entries.push((second, position | SECONDARY));
                }
                position += rec_len as u64;
            }
            let guard: Guard = rest[rest.len() - GUARD_LEN..].try_into().unwrap();
            runs.extend(&entries, self.len, guard, durable)?;
        }
        let keep = (self.len - runs.covered()) as usize;
        let kept = self.tail.split_off(self.tail.len() - keep);
        self.runs = runs;
        self.tail.clear();
        self.tail_keys.clear();
        self.tail_second.clear();
        self.push_tail(&kept);
        Ok(())
    }

    pub fn sync(&mut self) -> Result<(), StoreError> {
        if !crate::relaxed_durability() {
            self.fsync()?;
        }
        Ok(())
    }

    /// Raw fsync (no relaxed gate), for the group commit layer.
    pub fn fsync(&self) -> Result<(), StoreError> {
        self.file.sync_all()?;
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

    /// An independent fd to the log file, for an off-write-path fsync.
    pub fn sync_handle(&self) -> Result<File, StoreError> {
        Ok(self.file.try_clone()?)
    }

    /// Cuts the log back to `target_len` (an earlier `appended_len`) and
    /// fsyncs the cut. Index runs reaching past the target go first.
    pub fn rewind(&mut self, target_len: u64) -> Result<(), StoreError> {
        let drop = self.len.checked_sub(target_len);
        if target_len < HEADER_LEN as u64
            || !drop.is_some_and(|n| n.is_multiple_of(self.layout.rec_len as u64))
        {
            return Err(StoreError::Format(self.layout.bad_rewind));
        }
        self.runs.truncate(target_len)?;
        self.file.set_len(target_len)?;
        self.file.sync_all()?;
        self.load_tail(target_len, false)
    }
}

impl Drop for RecLog {
    fn drop(&mut self) {
        // best-effort durability; explicit sync() is the checked path
        if self.unsynced {
            let _ = self.file.sync_all();
        }
    }
}
