//! The boot run: one run past the chain, over records that were left
//! unsynced.
//!
//! The runs of the chain are built over synced records only, so what a
//! writer appends without an fsync stays in the tail every opener reads.
//! Until the machine goes down those records are in the kernel's cache as
//! they were written, and so is a file written beside them: a writer that
//! leaves its appends unsynced indexes them in one run of its own, which
//! begins where the chain ends and is written anew, with the records
//! appended since, each time enough of them have gathered.
//!
//! The file lies in a directory under the index directory that whoever
//! keeps the log names for the boot and for the state of the log the run
//! was written in (`<name>/<start:012x>-<end:012x>.run`), and it is never
//! fsynced. It holds for as long as that name does: after another boot,
//! or once records were taken out of the log, nobody opens it. An opener
//! is handed the name and the range ([`BootRun`]); the directory is not
//! read. A run added to the chain ends every boot run of the log: the
//! chain then ends elsewhere.

use std::fs;
use std::path::Path;

use super::{CORRUPT, ENTRY_LEN, Guard, KEY_LEN, Key, Run, Runs, run_name};
use crate::StoreError;
use crate::run_write::write_run;

/// Names a boot run: the directory it lies in under the index directory
/// of its log, and the log range it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootRun {
    pub dir: String,
    pub start: u64,
    pub end: u64,
}

/// The entries of a run for `entries`, in the order a run holds them.
pub(super) fn sorted(entries: &[(Key, u64)]) -> Vec<[u8; ENTRY_LEN]> {
    let mut sorted: Vec<[u8; ENTRY_LEN]> = entries
        .iter()
        .map(|(key, position)| {
            let mut entry = [0u8; ENTRY_LEN];
            entry[..KEY_LEN].copy_from_slice(key);
            entry[KEY_LEN..].copy_from_slice(&position.to_be_bytes());
            entry
        })
        .collect();
    sorted.sort_unstable();
    sorted
}

/// Removes a directory of boot runs; another process may have removed it
/// already.
pub(super) fn remove_dir(path: &Path) -> Result<(), StoreError> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Removes every directory of boot runs under the index directory `dir`
/// but the one named `keep`. The caller holds the log's write lock.
pub fn remove_boot_runs(dir: &Path, keep: Option<&str>) -> Result<(), StoreError> {
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in listing {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.file_name().to_str() != keep {
            remove_dir(&entry.path())?;
        }
    }
    Ok(())
}

impl Runs {
    fn boot(&self) -> Option<&Run> {
        let last = self.runs.last()?;
        (last.path.parent() != Some(self.dir.as_path())).then_some(last)
    }

    /// How many runs make up the chain.
    pub(super) fn chained(&self) -> usize {
        self.runs.len() - self.boot().is_some() as usize
    }

    /// The log position the chain ends at: where a boot run begins.
    pub fn chain_end(&self) -> u64 {
        let chain = &self.runs[..self.chained()];
        chain.last().map_or(self.base, |run| run.end)
    }

    /// The log range of the boot run held here.
    pub fn boot_run(&self) -> Option<(u64, u64)> {
        self.boot().map(|run| (run.start, run.end))
    }

    /// Takes the boot run `named`, when it begins where the chain ends
    /// and its guard matches the log (see [`Runs::open`] for `guard_at`).
    pub fn with_boot_run(
        mut self,
        named: Option<&BootRun>,
        guard_at: impl Fn(u64) -> Option<Guard>,
    ) -> Result<Self, StoreError> {
        let Some(named) = named.filter(|named| named.start == self.covered()) else {
            return Ok(self);
        };
        let name = run_name(named.start, named.end);
        let path = self.dir.join(&named.dir).join(name);
        if let Some(run) = Run::load(path, named.start, named.end)?
            && guard_at(run.end).as_deref() == Some(run.guard())
        {
            self.runs.push(run);
        }
        Ok(self)
    }

    /// No longer looks in the boot run. Its file stays.
    pub fn drop_boot_run(&mut self) {
        self.runs.truncate(self.chained());
    }

    /// Writes the boot run anew in the directory `dir` under the index
    /// directory: over what the one held here covers and the log range up
    /// to `end` after it, whose records `entries` holds, all of them.
    /// The caller holds the log's write lock. Every other directory of
    /// boot runs goes, and every other file of this one.
    pub fn extend_boot_run(
        &mut self,
        dir: &str,
        entries: &[(Key, u64)],
        end: u64,
        guard: Guard,
    ) -> Result<(), StoreError> {
        self.write_boot_run(dir, entries, end, guard)
            .map_err(|e| match e {
                StoreError::Io(e) => StoreError::IndexWrite(e),
                other => other,
            })
    }

    fn write_boot_run(
        &mut self,
        dir: &str,
        entries: &[(Key, u64)],
        end: u64,
        guard: Guard,
    ) -> Result<(), StoreError> {
        let fresh = sorted(entries);
        let start = self.chain_end();
        let total = entries.len() + self.boot().map_or(0, |run| run.count);
        let sub = self.dir.join(dir);
        fs::create_dir_all(&sub)?;
        let path = sub.join(run_name(start, end));
        let tmp = path.with_extension("tmp");
        {
            let mut sources: Vec<&[u8]> = self.boot().map(Run::entries).into_iter().collect();
            sources.push(fresh.as_flattened());
            write_run(&tmp, &mut sources, total, start, end, &guard, false)?;
        }
        fs::rename(&tmp, &path)?;
        let run = Run::load(path, start, end)?.ok_or(CORRUPT)?;
        self.drop_boot_run();
        self.runs.push(run);
        remove_boot_runs(&self.dir, Some(dir))?;
        for entry in fs::read_dir(&sub)? {
            let other = entry?.path();
            if self.boot().is_some_and(|run| run.path != other) {
                fs::remove_file(other)?;
            }
        }
        Ok(())
    }
}
