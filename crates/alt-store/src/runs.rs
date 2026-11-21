//! Sorted index runs over an append-only log: the on-disk lookup structure
//! that lets a store open without replaying its whole log.
//!
//! A run is an immutable file of `(key, position)` entries sorted by key,
//! covering one contiguous range `[start, end)` of log positions. The runs
//! of a log chain end to start from the log's first record; whatever lies
//! past the last run is the tail the opener still scans. Runs are a cache:
//! the log stays the truth, every hit is checked against the record it
//! points at, and a missing or unusable run only makes the tail longer.
//!
//! File `<start:012x>-<end:012x>.run`, integers little-endian unless noted:
//!
//! ```text
//! [magic "ALTR"][version u8][fanout bits u8][reserved 2]
//! [count u64][start u64][end u64][guard len u32][reserved 12]
//! [header checksum 8]
//! count x [key 32][position u64 big-endian]   sorted bytewise
//! (1 << fanout bits) x [u32]                  entries with prefix <= i
//! [guard]
//! [checksum 8]                                over everything after the header
//! ```
//!
//! The guard is a copy of the log bytes just before `end`, up to
//! [`GUARD_SPAN`] of them: a run whose guard no longer matches the log (the
//! log was cut back and regrown) is dropped. It cannot tell a log changed
//! only further back than that, which is why a hit is still checked against
//! the record it points at before anything is concluded from it.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use alt_os::map::Mmap;

use crate::StoreError;
use crate::run_write::write_run;

mod named;
pub use named::unusable;

pub const KEY_LEN: usize = 32;
/// How many log bytes before a run's end its guard holds, at most.
pub const GUARD_SPAN: usize = 4096;
pub type Key = [u8; KEY_LEN];
/// The log bytes just before a position: the last [`GUARD_SPAN`] of them,
/// or all there are.
pub type Guard = Vec<u8>;

pub(crate) const ENTRY_LEN: usize = KEY_LEN + 8;
pub(crate) const MAGIC: [u8; 4] = *b"ALTR";
pub(crate) const VERSION: u8 = 2;
pub(crate) const HEADER_LEN: usize = 56;
/// A new run absorbs every older neighbour smaller than this many times its
/// own size, so run counts stay logarithmic in the log length.
const MERGE_RATIO: usize = 8;

const CORRUPT: StoreError = StoreError::Format("index run corrupt");

struct Run {
    map: Mmap,
    path: PathBuf,
    start: u64,
    end: u64,
    count: usize,
    bits: u8,
    guard_len: usize,
}

fn run_name(start: u64, end: u64) -> String {
    format!("{start:012x}-{end:012x}.run")
}

fn parse_name(name: &str) -> Option<(u64, u64)> {
    let (start, end) = name.strip_suffix(".run")?.split_once('-')?;
    if start.len() != 12 || end.len() != 12 {
        return None;
    }
    Some((
        u64::from_str_radix(start, 16).ok()?,
        u64::from_str_radix(end, 16).ok()?,
    ))
}

pub(crate) fn check8(data: &[u8]) -> [u8; 8] {
    alt_hash::blake3::hash(data).as_bytes()[..8]
        .try_into()
        .unwrap()
}

impl Run {
    /// Maps a run file; `None` when it is not a complete run for this range.
    fn load(path: PathBuf, start: u64, end: u64) -> Result<Option<Self>, StoreError> {
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let map = unsafe { Mmap::map(&file)? };
        if map.len() < HEADER_LEN || map[..4] != MAGIC || map[4] != VERSION {
            return Ok(None);
        }
        if check8(&map[..48]) != map[48..56] {
            return Ok(None);
        }
        let bits = map[5];
        let count = u64::from_le_bytes(map[8..16].try_into().unwrap()) as usize;
        let head_start = u64::from_le_bytes(map[16..24].try_into().unwrap());
        let head_end = u64::from_le_bytes(map[24..32].try_into().unwrap());
        let guard_len = u32::from_le_bytes(map[32..36].try_into().unwrap()) as usize;
        let expect = count
            .checked_mul(ENTRY_LEN)
            .and_then(|n| n.checked_add(HEADER_LEN + (4usize << bits) + guard_len + 8));
        if !(bits == 8 || bits == 16)
            || guard_len > GUARD_SPAN
            || head_start != start
            || head_end != end
            || expect != Some(map.len())
        {
            return Ok(None);
        }
        Ok(Some(Self {
            map,
            path,
            start,
            end,
            count,
            bits,
            guard_len,
        }))
    }

    fn guard(&self) -> &[u8] {
        let end = self.map.len() - 8;
        &self.map[end - self.guard_len..end]
    }

    fn entries(&self) -> &[u8] {
        &self.map[HEADER_LEN..HEADER_LEN + self.count * ENTRY_LEN]
    }

    fn entry(&self, at: usize) -> &[u8] {
        &self.entries()[at * ENTRY_LEN..(at + 1) * ENTRY_LEN]
    }

    fn prefix(&self, key: &[u8]) -> usize {
        if self.bits == 8 {
            key[0] as usize
        } else {
            u16::from_be_bytes([key[0], key[1]]) as usize
        }
    }

    fn fanout(&self, prefix: usize) -> usize {
        let at = HEADER_LEN + self.count * ENTRY_LEN + prefix * 4;
        u32::from_le_bytes(self.map[at..at + 4].try_into().unwrap()) as usize
    }

    /// Index of the first entry not below `key`, searched inside the key's
    /// fanout bucket. The neighbours are checked against the bucket, so a
    /// damaged table or entry is reported instead of turning into a miss.
    fn lower_bound(&self, key: &Key) -> Result<usize, StoreError> {
        let prefix = self.prefix(key);
        let lo = if prefix == 0 {
            0
        } else {
            self.fanout(prefix - 1)
        };
        let hi = self.fanout(prefix);
        if lo > hi || hi > self.count {
            return Err(CORRUPT);
        }
        let (mut left, mut right) = (lo, hi);
        while left < right {
            let mid = left + (right - left) / 2;
            if self.entry(mid)[..KEY_LEN] < key[..] {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        if left > lo {
            let before = self.entry(left - 1);
            if self.prefix(before) != prefix || before[..KEY_LEN] >= key[..] {
                return Err(CORRUPT);
            }
        }
        if left < hi {
            let at = self.entry(left);
            if self.prefix(at) != prefix || at[..KEY_LEN] < key[..] {
                return Err(CORRUPT);
            }
        }
        Ok(left)
    }

    fn position(entry: &[u8]) -> u64 {
        u64::from_be_bytes(entry[KEY_LEN..].try_into().unwrap())
    }
}

/// The chain of runs indexing one log.
pub struct Runs {
    dir: PathBuf,
    base: u64,
    /// oldest first; each starts where the previous one ends
    runs: Vec<Run>,
    /// run files outside the chain, removed by the next writer
    stale: Vec<PathBuf>,
}

impl Runs {
    /// Loads the chain that starts at `base`. `guard_at(end)` returns the log
    /// bytes before `end` (see [`Guard`]), or `None` when the log does not
    /// reach that far.
    pub fn open(
        dir: &Path,
        base: u64,
        guard_at: impl Fn(u64) -> Option<Guard>,
    ) -> Result<Self, StoreError> {
        let mut found = named::listed(dir)?;
        let mut runs: Vec<Run> = Vec::new();
        let mut at = base;
        // the widest run from each point wins: it is the most merged one
        while let Some(pick) = found
            .iter()
            .enumerate()
            .filter(|(_, (start, end, _))| *start == at && *end > at)
            .max_by_key(|(_, (_, end, _))| *end)
            .map(|(i, _)| i)
        {
            let (start, end, path) = found.swap_remove(pick);
            match Run::load(path.clone(), start, end)? {
                Some(run) => {
                    at = run.end;
                    runs.push(run);
                }
                None => {
                    found.push((u64::MAX, 0, path));
                    break;
                }
            }
        }
        let stale = found.into_iter().map(|(_, _, path)| path).collect();
        Ok(Self::guarded(dir, base, runs, stale, guard_at))
    }

    /// The log position up to which records are indexed.
    pub fn covered(&self) -> u64 {
        self.runs.last().map_or(self.base, |run| run.end)
    }

    /// Entries across all runs (a key indexed twice counts twice).
    pub fn count(&self) -> usize {
        self.runs.iter().map(|run| run.count).sum()
    }

    pub fn run_count(&self) -> usize {
        self.runs.len()
    }

    /// The latest position recorded for `key`.
    pub fn find_last(&self, key: &Key) -> Result<Option<u64>, StoreError> {
        for run in self.runs.iter().rev() {
            let mut at = run.lower_bound(key)?;
            let mut last = None;
            while at < run.count && run.entry(at)[..KEY_LEN] == key[..] {
                last = Some(Run::position(run.entry(at)));
                at += 1;
            }
            if last.is_some() {
                return Ok(last);
            }
        }
        Ok(None)
    }

    /// Every position recorded for `key`, oldest first.
    pub fn find_each(&self, key: &Key, mut each: impl FnMut(u64)) -> Result<(), StoreError> {
        for run in &self.runs {
            let mut at = run.lower_bound(key)?;
            while at < run.count && run.entry(at)[..KEY_LEN] == key[..] {
                each(Run::position(run.entry(at)));
                at += 1;
            }
        }
        Ok(())
    }

    /// Visits entries with `lo <= key <= hi` until `each` returns false.
    pub fn range(
        &self,
        lo: &Key,
        hi: &Key,
        mut each: impl FnMut(&[u8], u64) -> bool,
    ) -> Result<(), StoreError> {
        for run in &self.runs {
            let mut at = run.lower_bound(lo)?;
            while at < run.count && run.entry(at)[..KEY_LEN] <= hi[..] {
                let entry = run.entry(at);
                if !each(&entry[..KEY_LEN], Run::position(entry)) {
                    return Ok(());
                }
                at += 1;
            }
        }
        Ok(())
    }

    /// Visits every entry, oldest run first.
    pub fn for_each(&self, mut each: impl FnMut(&[u8], u64)) {
        for run in &self.runs {
            for entry in run.entries().as_chunks::<ENTRY_LEN>().0 {
                each(&entry[..KEY_LEN], Run::position(entry));
            }
        }
    }

    /// Re-hashes every run against its trailer checksum.
    pub fn verify(&self) -> Result<(), StoreError> {
        for run in &self.runs {
            let body = &run.map[HEADER_LEN..run.map.len() - 8];
            if check8(body) != run.map[run.map.len() - 8..] {
                return Err(CORRUPT);
            }
        }
        Ok(())
    }

    /// Indexes the log range `[from, end)`, which must begin where the runs
    /// stop: `entries` holds every record in it. The caller holds the log's
    /// write lock. Older runs that the new one outgrows are merged into it.
    pub fn extend(
        &mut self,
        entries: &[(Key, u64)],
        from: u64,
        end: u64,
        guard: Guard,
        durable: bool,
    ) -> Result<(), StoreError> {
        if from != self.covered() {
            return Err(StoreError::Format("index run would leave records out"));
        }
        // the runs are a cache: what goes wrong writing one is told apart
        // from damage to the log
        self.write_next(entries, end, guard, durable)
            .map_err(|e| match e {
                StoreError::Io(e) => StoreError::IndexWrite(e),
                other => other,
            })
    }

    fn write_next(
        &mut self,
        entries: &[(Key, u64)],
        end: u64,
        guard: Guard,
        durable: bool,
    ) -> Result<(), StoreError> {
        self.remove_stale()?;
        let mut fresh: Vec<[u8; ENTRY_LEN]> = entries
            .iter()
            .map(|(key, position)| {
                let mut entry = [0u8; ENTRY_LEN];
                entry[..KEY_LEN].copy_from_slice(key);
                entry[KEY_LEN..].copy_from_slice(&position.to_be_bytes());
                entry
            })
            .collect();
        fresh.sort_unstable();
        let fresh = fresh.as_flattened();

        let mut keep = self.runs.len();
        let mut total = entries.len();
        while keep > 0 && self.runs[keep - 1].count < MERGE_RATIO * total {
            total += self.runs[keep - 1].count;
            keep -= 1;
        }
        let start = self.runs.get(keep).map_or(self.covered(), |run| run.start);

        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(run_name(start, end));
        let tmp = path.with_extension("run.tmp");
        {
            let mut sources: Vec<&[u8]> = self.runs[keep..].iter().map(Run::entries).collect();
            sources.push(fresh);
            write_run(&tmp, &mut sources, total, start, end, &guard, durable)?;
        }
        fs::rename(&tmp, &path)?;
        // nothing changes in memory until the new run is in hand
        let run = Run::load(path, start, end)?.ok_or(CORRUPT)?;
        drop(self.runs.drain(keep..));
        self.runs.push(run);
        // the merged runs go, and with them anything a dead writer left
        self.sweep()
    }

    /// Where the runs [`Self::truncate`] would keep for `target` stop.
    pub fn covered_up_to(&self, target: u64) -> u64 {
        let kept = self.runs.iter().take_while(|run| run.end <= target);
        kept.last().map_or(self.base, |run| run.end)
    }

    /// For a handle that holds the log as ending at `target`: no longer
    /// looks in the runs that reach past it. Their files stay.
    pub fn pass_over_past(&mut self, target: u64) {
        self.runs.retain(|run| run.end <= target);
    }

    /// Drops the runs that reach past `target`, before the log is cut back
    /// to it: every such file in the directory, not only the ones this
    /// handle loaded (another process may have merged them since). The
    /// removal is made durable so a dropped run cannot reappear under a log
    /// that has since regrown.
    pub fn truncate(&mut self, target: u64) -> Result<(), StoreError> {
        self.remove_stale()?;
        self.runs.retain(|run| run.end <= target);
        let listing = match fs::read_dir(&self.dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let mut removed = false;
        for entry in listing {
            let entry = entry?;
            let past = entry
                .file_name()
                .to_str()
                .and_then(parse_name)
                .is_some_and(|(_, end)| end > target);
            if past {
                remove_if_present(&entry.path())?;
                removed = true;
            }
        }
        if removed {
            crate::pack::fsync_dir(&self.dir)?;
        }
        Ok(())
    }

    /// Removes run files left outside the chain (a crash mid-merge, or a log
    /// cut back behind them). The caller holds the log's write lock.
    pub fn remove_stale(&mut self) -> Result<(), StoreError> {
        if self.stale.is_empty() {
            return Ok(());
        }
        for path in self.stale.drain(..) {
            remove_if_present(&path)?;
        }
        crate::pack::fsync_dir(&self.dir)?;
        Ok(())
    }
}

/// Another process holding the lock earlier may have removed it already.
fn remove_if_present(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}
