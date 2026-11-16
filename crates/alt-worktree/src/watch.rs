//! A working tree watched for changes, for a process that scans it many
//! times: after one full scan, only the paths the system reported events
//! for are looked at again. Whenever the events cannot be trusted to be
//! complete, the tree is scanned in full.

use std::io;
use std::path::{Path, PathBuf};

use crate::delta::{Delta, ScanOptions};
use crate::snapshot::{FileStat, Snapshot};
use held::{Cached, merged, stats, sure};
use paths::{beneath, tracked};

mod dirs;
mod held;
mod mounts;
mod moved;
#[path = "watch/linux.rs"]
mod os;
mod paths;
mod scans;

/// What happened in the tree since the watcher was last asked.
pub(crate) enum Changes {
    /// these paths, relative to the root, each with everything beneath it
    Paths(Vec<Vec<u8>>),
    /// the tree was rearranged and is watched anew: anything may differ
    All,
    /// events were lost: anything may differ, and directories may have
    /// come to be that are not watched
    Unknown,
}

/// The most watches one tree may take: half of what the system allows the
/// user, which every tree and every other program of theirs draws on.
pub fn watch_allowance() -> io::Result<usize> {
    let all = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")?;
    let all: usize = all.trim().parse().map_err(io::Error::other)?;
    Ok(all / 2)
}

/// Why a watched tree has no answer.
#[derive(Debug, PartialEq, Eq)]
pub enum Unanswered {
    /// this scan failed: the tree is watched as before, and the next
    /// scan looks at all of it
    ScanFailed,
    /// the tree can no longer be watched, and the value is to be dropped
    Unwatched,
}

pub struct Watched {
    watcher: os::Watcher,
    /// the index the watcher last chose its directories by
    index_id: Vec<u8>,
    cached: Option<Cached>,
    /// the ignore files outside the tree that the watcher read its rules
    /// from
    rule_files: Vec<PathBuf>,
    /// those files as they were then; `None` when one of them can change
    /// and look the same
    rules: Option<Vec<Option<FileStat>>>,
    /// what was taken off the queue between two scans
    absorbed: Option<Changes>,
    /// the queue could not be read: the tree is no longer watched
    failed: bool,
    /// watching ended for want of watches
    full: bool,
    /// how often the whole tree was scanned
    whole: usize,
}

impl Watched {
    /// Starts watching the tree at `root`, which takes one watch for every
    /// directory a scan of it for `snapshot` looks into; a tree with more
    /// than `most` of them is not watched, and fails the way a tree does
    /// that the system has no watches left for. Fails as well for a tree
    /// that events do not tell everything about. The first scan sees the
    /// whole tree, and it runs after every directory is being watched.
    /// `opts` are those of the scan that follows.
    pub fn start(
        root: &Path,
        most: usize,
        snapshot: &Snapshot,
        opts: &ScanOptions<'_>,
    ) -> io::Result<Watched> {
        // the stat first: a file replaced while it is read differs next time
        let rules = stats(opts.excludes);
        let tracked = |dir: &[u8]| tracked(snapshot, dir);
        let (watcher, whole) = os::Watcher::start(root, most, &tracked, opts.excludes)?;
        Ok(Watched {
            watcher,
            index_id: snapshot.index_id.clone(),
            cached: None,
            rule_files: opts
                .excludes
                .iter()
                .map(|file| file.to_path_buf())
                .collect(),
            rules: sure(rules, &opts.clock),
            absorbed: (!whole).then_some(Changes::Unknown),
            failed: false,
            full: false,
            whole: 0,
        })
    }

    /// Whether watching this tree ended because no watches were left for
    /// it, which no later attempt changes.
    pub fn out_of_watches(&self) -> bool {
        self.full
    }

    /// `result`, with a note of it when it is the failure for want of
    /// watches.
    fn noting<T>(&mut self, result: io::Result<T>) -> Option<T> {
        if let Err(e) = &result {
            self.full |= e.raw_os_error() == Some(os::ENOSPC);
        }
        result.ok()
    }

    /// How often the watches were set up from the root: once at the start,
    /// and again each time events were lost.
    pub fn set_up(&self) -> usize {
        self.watcher.set_up
    }

    /// How often a scan looked at the whole tree.
    pub fn scanned_whole(&self) -> usize {
        self.whole
    }

    /// For a process that put the index of `next` where that of `prior`
    /// stood: what is held as the answer for the one is held for the
    /// other, and the paths whose entries differ are looked at by the
    /// next scan. Without this, that scan looks at the whole tree.
    pub fn index_replaced(&mut self, prior: &Snapshot, next: &Snapshot) {
        let Some(cached) = &mut self.cached else {
            return;
        };
        if cached.index_id != prior.index_id {
            return;
        }
        cached.index_id.clone_from(&next.index_id);
        let differing = Changes::Paths(moved::between(prior, next));
        self.absorbed = Some(merged(self.absorbed.take(), differing));
    }

    /// Drops the answer held: the next scan looks at the whole tree.
    pub fn forget(&mut self) {
        self.cached = None;
    }

    /// Takes the events queued so far off the queue, to be answered for by
    /// the next scan. For a process that writes many files to the tree
    /// itself, more than the queue holds: called every so often while it
    /// does, it keeps the queue from running over.
    pub fn absorb(&mut self) {
        match self.watcher.absorb() {
            Ok(paths) => self.absorbed = Some(merged(self.absorbed.take(), Changes::Paths(paths))),
            Err(_) => self.failed = true,
        }
    }

    /// What happened in the tree since this was last asked, the watches
    /// brought in line with the ignore rules and the index of `snapshot`
    /// first.
    fn changes(&mut self, snapshot: &Snapshot, opts: &ScanOptions<'_>) -> Option<Changes> {
        let tracked = |dir: &[u8]| tracked(snapshot, dir);
        let rules = stats(opts.excludes);
        let same_files = self
            .rule_files
            .iter()
            .map(PathBuf::as_path)
            .eq(opts.excludes.iter().copied());
        if !same_files || self.rules.as_ref() != Some(&rules) {
            // other rules hide other directories
            let anew = self.watcher.start_over(&tracked, opts.excludes);
            let Some(whole) = self.noting(anew)? else {
                // the rules are read again the next time
                self.rules = None;
                return Some(Changes::Unknown);
            };
            self.rule_files = opts
                .excludes
                .iter()
                .map(|file| file.to_path_buf())
                .collect();
            self.rules = sure(rules, &opts.clock);
            self.index_id.clone_from(&snapshot.index_id);
            self.absorbed = None;
            return Some(match whole {
                true => Changes::All,
                false => Changes::Unknown,
            });
        }
        if self.index_id != snapshot.index_id {
            let more = self.watcher.revisit(&tracked);
            if !self.noting(more)? {
                self.absorbed = Some(Changes::Unknown);
            }
            self.index_id.clone_from(&snapshot.index_id);
        }
        let fresh = self.watcher.changes(&tracked);
        let fresh = self.noting(fresh)?;
        Some(merged(self.absorbed.take(), fresh))
    }

    /// What [`scan`](crate::scan) of the whole tree returns for `opts`,
    /// which ask for no untracked files or for directories as one path.
    /// `settings` are the files outside the tree that `opts` read what
    /// they do with a file from, and `context` whatever else they go by:
    /// a change in either is answered by a scan of everything. Where
    /// there is no answer, the caller scans the tree itself, which
    /// answers or fails as it does for a tree that is not watched.
    pub fn scan(
        &mut self,
        root: &Path,
        snapshot: &mut Snapshot,
        opts: &ScanOptions<'_>,
        settings: &[&Path],
        context: &[u8],
    ) -> Result<Delta, Unanswered> {
        if self.failed {
            return Err(Unanswered::Unwatched);
        }
        // before anything is looked at: an event that comes later is seen next time
        let changes = self.changes(snapshot, opts).ok_or(Unanswered::Unwatched)?;
        let outside = stats(settings);
        // taken out: a scan that fails leaves nothing to build on
        let held = self.cached.take().filter(|cached| {
            cached.index_id == snapshot.index_id
                && cached.outside.as_ref() == Some(&outside)
                && cached.context == context
        });
        let delta = match (changes, held) {
            (Changes::Unknown, _) => self.scan_after_loss(root, snapshot, opts)?,
            (Changes::Paths(paths), Some(held)) => {
                self.scan_paths(root, snapshot, opts, paths, held.delta)?
            }
            _ => self.scan_whole(root, snapshot, opts)?,
        };
        Ok(self.keep(snapshot, opts, outside, context, delta))
    }
}
