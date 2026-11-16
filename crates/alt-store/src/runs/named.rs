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
        let mut stale = Vec::new();
        while let Some(last) = runs.last() {
            if guard_at(last.end).as_deref() == Some(last.guard()) {
                break;
            }
            stale.push(runs.pop().unwrap().path);
        }
        Ok(Self {
            dir: dir.to_owned(),
            base,
            runs,
            stale,
        })
    }

    /// The log range of each run, oldest first: what [`Runs::open_named`]
    /// takes.
    pub fn chain(&self) -> Vec<(u64, u64)> {
        self.runs.iter().map(|run| (run.start, run.end)).collect()
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
            }
        }
        if removed {
            crate::pack::fsync_dir(&self.dir)?;
        }
        Ok(())
    }
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
