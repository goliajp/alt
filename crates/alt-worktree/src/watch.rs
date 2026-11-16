//! A working tree watched for changes, for a process that scans it many
//! times: after one full scan, only the paths the system reported events
//! for are looked at again. Whenever the events cannot be trusted to be
//! complete, the tree is scanned in full.

use std::io;
use std::path::{Path, PathBuf};

use crate::clock::Clock;
use crate::delta::{Delta, ScanOptions};
use crate::snapshot::{FileStat, Snapshot};
use crate::untracked::Untracked;
use paths::{beneath, dirty_paths, tracked};

mod dirs;
mod mounts;
#[path = "watch/linux.rs"]
mod os;
mod paths;

/// More changed paths than this are answered by a full scan.
const MANY_PATHS: usize = 256;

/// What happened in the tree since the watcher was last asked.
pub(crate) enum Changes {
    /// these paths, relative to the root, each with everything beneath it
    Paths(Vec<Vec<u8>>),
    /// events were lost or the tree was rearranged: anything may differ
    All,
}

/// The most watches one tree may take: half of what the system allows the
/// user, which every tree and every other program of theirs draws on.
pub fn watch_allowance() -> io::Result<usize> {
    let all = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")?;
    let all: usize = all.trim().parse().map_err(io::Error::other)?;
    Ok(all / 2)
}

struct Cached {
    index_id: Vec<u8>,
    /// the files outside the tree that decide what a scan answers, as
    /// they were; `None` when one of them can change and look the same
    outside: Option<Vec<Option<FileStat>>>,
    /// what else decides it, as the caller put it
    context: Vec<u8>,
    delta: Delta,
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
        Ok(Watched {
            watcher: os::Watcher::start(root, most, &tracked, opts.excludes)?,
            index_id: snapshot.index_id.clone(),
            cached: None,
            rule_files: opts
                .excludes
                .iter()
                .map(|file| file.to_path_buf())
                .collect(),
            rules: sure(rules, &opts.clock),
            absorbed: None,
            failed: false,
        })
    }

    /// How often the watches were set up from the root: once at the start,
    /// and again each time events were lost.
    pub fn set_up(&self) -> usize {
        self.watcher.set_up
    }

    /// Takes the events queued so far off the queue, to be answered for by
    /// the next scan. For a process that writes many files to the tree
    /// itself, more than the queue holds: called every so often while it
    /// does, it keeps the queue from running over.
    pub fn absorb(&mut self) {
        // the index is being replaced: every new directory is watched
        match self.watcher.changes(&|_| true) {
            Ok(more) => self.absorbed = Some(merged(self.absorbed.take(), more)),
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
            self.watcher.start_over(&tracked, opts.excludes).ok()?;
            self.rule_files = opts
                .excludes
                .iter()
                .map(|file| file.to_path_buf())
                .collect();
            self.rules = sure(rules, &opts.clock);
            self.index_id.clone_from(&snapshot.index_id);
            self.absorbed = None;
            return Some(Changes::All);
        }
        if self.index_id != snapshot.index_id {
            self.watcher.revisit(&tracked).ok()?;
            self.index_id.clone_from(&snapshot.index_id);
        }
        let fresh = self.watcher.changes(&tracked).ok()?;
        Some(merged(self.absorbed.take(), fresh))
    }

    /// What [`scan`](crate::scan) of the whole tree returns for `opts`,
    /// which ask for no untracked files or for directories as one path.
    /// `settings` are the files outside the tree that `opts` read what
    /// they do with a file from, and `context` whatever else they go by:
    /// a change in either is answered by a scan of everything. `None` when the tree can no longer be watched
    /// or a scan of it failed: the caller scans it itself, which answers
    /// or fails as it does for a tree that is not watched, and drops this
    /// value.
    pub fn scan(
        &mut self,
        root: &Path,
        snapshot: &mut Snapshot,
        opts: &ScanOptions<'_>,
        settings: &[&Path],
        context: &[u8],
    ) -> Option<Delta> {
        if self.failed {
            return None;
        }
        // before anything is looked at: an event that comes later is seen next time
        let changes = self.changes(snapshot, opts)?;
        let outside = stats(settings);
        // taken out: a scan that fails leaves nothing to build on
        let cached = self.cached.take();
        let whole = |within| ScanOptions {
            untracked: Untracked::Normal,
            within,
            clean: opts.clean,
            head: opts.head,
            excludes: opts.excludes,
            clock: opts.clock,
        };
        let dirty = match changes {
            Changes::Paths(paths) => Some(dirty_paths(paths, snapshot)),
            Changes::All => None,
        };
        let delta = match (dirty, cached) {
            (Some(dirty), Some(cached))
                if cached.index_id == snapshot.index_id
                    && cached.outside.as_ref() == Some(&outside)
                    && cached.context == context
                    && dirty.len() <= MANY_PATHS
                    && dirty.iter().all(|p| !p.is_empty()) =>
            {
                let mut delta = cached.delta;
                if !dirty.is_empty() {
                    let within: Vec<&[u8]> = dirty.iter().map(Vec::as_slice).collect();
                    let fresh = crate::scan(root, snapshot, &whole(&within)).ok()?;
                    delta.replace_within(&within, fresh);
                }
                delta
            }
            _ => crate::scan(root, snapshot, &whole(&[])).ok()?,
        };
        self.cached = Some(Cached {
            index_id: snapshot.index_id.clone(),
            outside: sure(outside, &opts.clock),
            context: context.to_vec(),
            delta: delta.clone(),
        });
        let mut delta = delta;
        if opts.untracked == Untracked::No {
            delta.untracked.clear();
        }
        Some(delta)
    }
}

/// What `earlier` and `later` tell together; more paths than a scan of
/// them is worth stand for the whole tree.
fn merged(earlier: Option<Changes>, later: Changes) -> Changes {
    match (earlier, later) {
        (None, later) => later,
        (Some(Changes::Paths(mut paths)), Changes::Paths(more)) => {
            paths.extend(more);
            match paths.len() > 4 * MANY_PATHS {
                true => Changes::All,
                false => Changes::Paths(paths),
            }
        }
        _ => Changes::All,
    }
}

/// What each of the files looks like now; `None` for one that is not there.
fn stats(files: &[&Path]) -> Vec<Option<FileStat>> {
    let stat = |path: &&Path| std::fs::metadata(path).ok().map(|m| FileStat::of(&m));
    files.iter().map(stat).collect()
}

/// `stats` as something to tell a later change by: `None` when one of the
/// files was written in the tick `clock` was read in or after it, since
/// it can be written again in that tick and look the same.
fn sure(stats: Vec<Option<FileStat>>, clock: &Clock) -> Option<Vec<Option<FileStat>>> {
    let settled = stats.iter().flatten().all(|stat| clock.settled(stat));
    settled.then_some(stats)
}
