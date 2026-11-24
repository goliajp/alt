//! The scans behind the answer of a watched tree.

use std::path::Path;

use super::held::{Cached, merged, replace_within, sure};
use super::paths::{dirty_paths, tracked};
use super::{Changes, Unanswered, Watched};
use crate::delta::{Delta, ScanOptions};
use crate::snapshot::{FileStat, Snapshot};
use crate::untracked::Untracked;

/// The scan whose answer is held, whatever of it `opts` ask for: with
/// directories as one path, of the paths `within`.
fn held_for<'a>(opts: &ScanOptions<'a>, within: &'a [&'a [u8]]) -> ScanOptions<'a> {
    ScanOptions {
        file_mode: opts.file_mode,
        ignore_case: opts.ignore_case,
        precompose: opts.precompose,
        untracked: Untracked::Normal,
        within,
        clean: opts.clean,
        head: opts.head,
        excludes: opts.excludes,
        clock: opts.clock,
    }
}

impl Watched {
    pub(super) fn scan_whole(
        &mut self,
        root: &Path,
        snapshot: &mut Snapshot,
        opts: &ScanOptions<'_>,
    ) -> Result<Delta, Unanswered> {
        self.whole += 1;
        crate::scan(root, snapshot, &held_for(opts, &[])).map_err(|_| Unanswered::ScanFailed)
    }

    /// `delta`, which held before something happened at `paths`, with
    /// what a scan of those finds now.
    pub(super) fn scan_paths(
        &mut self,
        root: &Path,
        snapshot: &mut Snapshot,
        opts: &ScanOptions<'_>,
        paths: Vec<Vec<u8>>,
        mut delta: Delta,
    ) -> Result<Delta, Unanswered> {
        let dirty = dirty_paths(paths, snapshot);
        if dirty.iter().any(|path| path.is_empty()) {
            return self.scan_whole(root, snapshot, opts);
        }
        if !dirty.is_empty() {
            let within: Vec<&[u8]> = dirty.iter().map(Vec::as_slice).collect();
            let fresh = crate::scan(root, snapshot, &held_for(opts, &within))
                .map_err(|_| Unanswered::ScanFailed)?;
            replace_within(&mut delta, &within, fresh);
        }
        Ok(delta)
    }

    /// Holds `delta` as the answer for the tree as it is, and returns what
    /// of it `opts` ask for.
    pub(super) fn keep(
        &mut self,
        snapshot: &Snapshot,
        opts: &ScanOptions<'_>,
        outside: Vec<Option<FileStat>>,
        context: &[u8],
        mut delta: Delta,
    ) -> Delta {
        self.cached = Some(Cached {
            index_id: snapshot.index_id.clone(),
            outside: sure(outside, &opts.clock),
            context: context.to_vec(),
            delta: delta.clone(),
        });
        if opts.untracked == Untracked::No {
            delta.untracked.clear();
        }
        delta
    }

    /// The scan after events were lost: of everything, and the watches
    /// are brought in line with the directories it came by. A directory
    /// that had no watch while it was listed is looked at again next time.
    pub(super) fn scan_after_loss(
        &mut self,
        root: &Path,
        snapshot: &mut Snapshot,
        opts: &ScanOptions<'_>,
    ) -> Result<Delta, Unanswered> {
        self.whole += 1;
        let scanned = crate::delta::scan_seeing(root, snapshot, &held_for(opts, &[]));
        let Ok((delta, seen)) = scanned else {
            // the watches are still to be gone over
            self.absorbed = Some(Changes::Unknown);
            return Err(Unanswered::ScanFailed);
        };
        let fresh = self.watcher.fill(seen, &|dir| tracked(snapshot, dir));
        match self.noting(fresh).ok_or(Unanswered::Unwatched)? {
            Some(fresh) if fresh.is_empty() => {}
            Some(fresh) => {
                self.absorbed = Some(merged(self.absorbed.take(), Changes::Paths(fresh)));
            }
            // a directory the scan did not read cannot be: the watches
            // are gone over again after the next scan, of everything
            None => self.absorbed = Some(Changes::Unknown),
        }
        Ok(delta)
    }
}
