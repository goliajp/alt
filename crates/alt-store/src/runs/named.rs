//! Opening a run chain from its recorded names instead of a directory
//! listing, and keeping the directory free of files outside the chain.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Guard, Run, Runs, parse_name, remove_if_present, run_name};
use crate::StoreError;

impl Runs {
    /// Like [`Runs::open`], for a caller that already knows which runs make
    /// up the chain (a store records them for its next opener): the files
    /// are opened by name and the directory is not read. Names that do not
    /// lead to a chain from `base` — the record is out of date — fall back
    /// to reading the directory.
    pub fn open_named(
        dir: &Path,
        base: u64,
        named: &[(u64, u64)],
        guard_at: impl Fn(u64) -> Option<Guard>,
    ) -> Result<Self, StoreError> {
        let mut runs: Vec<Run> = Vec::with_capacity(named.len());
        let mut at = base;
        for &(start, end) in named {
            let run = if start == at && end > at {
                Run::load(dir.join(run_name(start, end)), start, end)?
            } else {
                None
            };
            let Some(run) = run else {
                return Self::open(dir, base, guard_at);
            };
            at = run.end;
            runs.push(run);
        }
        Ok(Self::guarded(dir, base, runs, Vec::new(), guard_at))
    }

    /// The chain `runs` without the runs at its end whose guard no longer
    /// matches the log; those join `stale`.
    pub(super) fn guarded(
        dir: &Path,
        base: u64,
        mut runs: Vec<Run>,
        mut stale: Vec<PathBuf>,
        guard_at: impl Fn(u64) -> Option<Guard>,
    ) -> Self {
        while let Some(last) = runs.last() {
            if guard_at(last.end).as_deref() == Some(last.guard()) {
                break;
            }
            stale.push(runs.pop().unwrap().path);
        }
        Self {
            dir: dir.to_owned(),
            base,
            runs,
            stale,
        }
    }

    /// The log range of each run, oldest first: what [`Runs::open_named`]
    /// takes.
    pub fn chain(&self) -> Vec<(u64, u64)> {
        let chain = &self.runs[..self.chained()];
        chain.iter().map(|run| (run.start, run.end)).collect()
    }

    /// Removes every run file, finished or not, that is not part of the
    /// chain. The caller holds the log's write lock, under which all run
    /// files are written: anything else in the directory is left over.
    pub(super) fn sweep(&mut self) -> Result<(), StoreError> {
        self.stale.clear();
        let mut removed = false;
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let ours = name.to_str().and_then(parse_name).is_some()
                || name.as_encoded_bytes().ends_with(b".run.tmp");
            let path = entry.path();
            if ours && !self.runs.iter().any(|run| run.path == path) {
                remove_if_present(&path)?;
                removed = true;
            } else if entry.file_type()?.is_dir() {
                // a boot run begins where the chain ended before this
                // run was written
                super::boot::remove_dir(&path)?;
            }
        }
        if removed {
            crate::pack::fsync_dir(&self.dir)?;
        }
        Ok(())
    }
}

/// Whether a run is fsynced before it takes the place of others. A run
/// is over records that were synced, and a search that misses in it is
/// taken at its word: one that a power cut left in part would hide
/// records that are there.
pub fn written_synced() -> bool {
    !alt_os::durability::never_synced()
}

/// The run files in `dir` by the log range each is named for; one that
/// was never finished is listed past every range.
pub(super) fn listed(dir: &Path) -> Result<Vec<(u64, u64, PathBuf)>, StoreError> {
    let mut found = Vec::new();
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(e.into()),
    };
    for entry in listing {
        let entry = entry?;
        let name = entry.file_name();
        if let Some((start, end)) = name.to_str().and_then(parse_name) {
            found.push((start, end, entry.path()));
        } else if name.as_encoded_bytes().ends_with(b".run.tmp") {
            // runs are written under the log's write lock, so one seen
            // here was left by a writer that died
            found.push((u64::MAX, 0, entry.path()));
        }
    }
    Ok(found)
}

/// The run files in `dir` that are not complete runs: a damaged header, a
/// file cut short or left empty. Every open passes them over and the next
/// writer replaces them.
pub fn unusable(dir: &Path) -> Result<Vec<PathBuf>, StoreError> {
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in listing {
        let entry = entry?;
        if let Some((start, end)) = entry.file_name().to_str().and_then(parse_name)
            && Run::load(entry.path(), start, end)?.is_none()
        {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

/// Removes every run file in `dir` that reaches past byte `target` of its
/// log, and fsyncs the removal: a run that is gone cannot come back under
/// records that were since written there.
pub fn remove_past(dir: &Path, target: u64) -> Result<(), StoreError> {
    let listing = match fs::read_dir(dir) {
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
        crate::pack::fsync_dir(dir)?;
    }
    Ok(())
}
