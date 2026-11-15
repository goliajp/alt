//! The walk through history: which commit each line comes from.

use std::collections::BinaryHeap;
use std::rc::Rc;

use alt_bytes::BString;
use alt_diff::Edit;
use alt_diff::fold::FoldState;
use alt_git_codec::{ObjectId, ObjectKind};
use alt_repo::Repository;

use super::Res;

/// Maps keyed by object ids, which need no careful hashing.
type HashMap<K, V> = std::collections::HashMap<K, V, FoldState>;
use crate::tree_changes::{self, TreeFile};

/// A run of lines of the blamed file that are, so far, traced to the same
/// run of lines of one version of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    /// first line in the blamed file
    pub(super) line: usize,
    /// first line in the version it is traced to
    pub(super) at: usize,
    pub(super) len: usize,
}

/// A file as one commit has it.
pub(super) struct Origin {
    pub(super) commit: ObjectId,
    pub(super) path: BString,
    pub(super) file: TreeFile,
    /// lines still looking for their commit, ordered by `at`
    pub(super) pending: Vec<Entry>,
}

pub(super) struct CommitInfo {
    pub(super) tree: ObjectId,
    pub(super) parents: Vec<ObjectId>,
    date: i64,
}

/// What to look for beyond changes in place and renames.
#[derive(Clone, Copy)]
pub struct Options {
    /// lines moved within the file (`-M`)
    pub moves: bool,
    /// lines copied from other files, as often as `-C` was given: from
    /// files changed in the same commit, then from any file when the
    /// blamed file is created, then from any file in every commit
    pub copies: u8,
    /// how many letters and digits moved lines must carry to be traced
    pub move_score: usize,
    /// the same for copied lines
    pub copy_score: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            moves: false,
            copies: 0,
            move_score: 20,
            copy_score: 40,
        }
    }
}

impl Options {
    /// The options `-M` and `-C` ask for, each a list of what was written
    /// against the letter each time it was given: nothing, or a score. As
    /// in git the last score counts, and one that is no number or zero
    /// leaves the usual score in place.
    pub fn of(moves: &[String], copies: &[String]) -> Self {
        let score = |given: &[String], usual: usize| {
            let last = given.last().and_then(|text| text.parse::<usize>().ok());
            last.filter(|score| *score > 0).unwrap_or(usual)
        };
        let usual = Options::default();
        Options {
            moves: !moves.is_empty() || !copies.is_empty(),
            copies: u8::try_from(copies.len()).unwrap_or(u8::MAX),
            move_score: score(moves, usual.move_score),
            copy_score: score(copies, usual.copy_score),
        }
    }
}

pub(super) struct Board<'r> {
    pub(super) repo: &'r Repository,
    pub(super) options: Options,
    pub(super) origins: Vec<Origin>,
    by_key: HashMap<(ObjectId, BString), usize>,
    by_commit: HashMap<ObjectId, Vec<usize>>,
    commits: HashMap<ObjectId, Rc<CommitInfo>>,
    /// commits with pending lines: newest first, then first queued
    queue: BinaryHeap<(i64, std::cmp::Reverse<u64>, ObjectId)>,
    queued: u64,
    blobs: HashMap<ObjectId, Rc<Vec<u8>>>,
    /// the blamed file, and where each of its lines starts
    pub(super) content: Rc<Vec<u8>>,
    pub(super) starts: Vec<usize>,
}

impl<'r> Board<'r> {
    pub(super) fn new(repo: &'r Repository, options: Options) -> Self {
        Board {
            repo,
            options,
            origins: Vec::new(),
            by_key: HashMap::default(),
            by_commit: HashMap::default(),
            commits: HashMap::default(),
            queue: BinaryHeap::new(),
            queued: 0,
            blobs: HashMap::default(),
            content: Rc::default(),
            starts: Vec::new(),
        }
    }

    /// The content of `path` at `start`, and the commit of each of its lines.
    pub(super) fn blame(
        mut self,
        start: ObjectId,
        path: &str,
    ) -> Res<(Rc<Vec<u8>>, Vec<ObjectId>)> {
        let path = path.trim_matches('/');
        let tree = self.commit(start)?.tree;
        let file = tree_changes::file_at(self.repo, tree, path.as_bytes())?
            .filter(|f| f.mode & 0o170000 != 0o160000)
            .ok_or_else(|| format!("path '{path}' not found in {start}"))?;
        self.content = self.blob(file.oid)?;
        self.starts = line_starts(&self.content);
        let lines = self.starts.len() - 1;
        let first = self.origin(start, BString::from(path), file);
        let whole = Entry {
            line: 0,
            at: 0,
            len: lines,
        };
        self.receive(first, vec![whole])?;

        let mut found = vec![start; lines];
        while let Some((_, _, commit)) = self.queue.pop() {
            // one commit can hold the file under several paths
            while let Some(at) = self.by_commit[&commit]
                .iter()
                .copied()
                .find(|&o| !self.origins[o].pending.is_empty())
            {
                self.pass_to_parents(at)?;
                for e in std::mem::take(&mut self.origins[at].pending) {
                    found[e.line..e.line + e.len].fill(commit);
                }
                self.blobs.remove(&self.origins[at].file.oid);
            }
        }
        Ok((self.content, found))
    }

    pub(super) fn commit(&mut self, oid: ObjectId) -> Res<Rc<CommitInfo>> {
        if let Some(info) = self.commits.get(&oid) {
            return Ok(info.clone());
        }
        let obj = self
            .repo
            .read_object(&oid)?
            .ok_or_else(|| format!("commit {oid} missing"))?;
        if obj.kind != ObjectKind::Commit {
            return Err(format!("{oid} is not a commit").into());
        }
        let info =
            Rc::new(commit_info(&obj.data).ok_or_else(|| format!("commit {oid} is malformed"))?);
        self.commits.insert(oid, info.clone());
        Ok(info)
    }

    pub(super) fn blob(&mut self, oid: ObjectId) -> Res<Rc<Vec<u8>>> {
        if let Some(bytes) = self.blobs.get(&oid) {
            return Ok(bytes.clone());
        }
        let obj = self
            .repo
            .read_object(&oid)?
            .ok_or_else(|| format!("blob {oid} missing"))?;
        if obj.kind != ObjectKind::Blob {
            return Err(format!("{oid} is not a blob").into());
        }
        let bytes = Rc::new(obj.data);
        self.blobs.insert(oid, bytes.clone());
        Ok(bytes)
    }

    pub(super) fn origin(&mut self, commit: ObjectId, path: BString, file: TreeFile) -> usize {
        let next = self.origins.len();
        let at = *self.by_key.entry((commit, path.clone())).or_insert(next);
        if at == next {
            self.by_commit.entry(commit).or_default().push(at);
            self.origins.push(Origin {
                commit,
                path,
                file,
                pending: Vec::new(),
            });
        }
        at
    }

    /// Adds pending lines to an origin, queueing its commit when these
    /// are its first.
    pub(super) fn receive(&mut self, at: usize, entries: Vec<Entry>) -> Res<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let origin = &mut self.origins[at];
        let first = origin.pending.is_empty();
        origin.pending.extend(entries);
        origin.pending.sort_unstable_by_key(|e| e.at);
        if first {
            let commit = origin.commit;
            let date = self.commit(commit)?.date;
            self.queue
                .push((date, std::cmp::Reverse(self.queued), commit));
            self.queued += 1;
        }
        Ok(())
    }

    /// Hands the pending lines of an origin to the parents they came from.
    /// What is left pending afterwards was introduced by its commit.
    fn pass_to_parents(&mut self, at: usize) -> Res<()> {
        let (commit, path, file) = {
            let o = &self.origins[at];
            (o.commit, o.path.clone(), o.file)
        };
        let info = self.commit(commit)?;
        let mut takers: Vec<Option<usize>> = vec![None; info.parents.len()];
        let mut has_path = vec![false; info.parents.len()];
        // the same path in each parent first, then a rename where a parent
        // lacks the path
        for renamed in [false, true] {
            for (i, &parent) in info.parents.iter().enumerate() {
                if takers[i].is_some() || has_path[i] {
                    continue;
                }
                let parent_tree = self.commit(parent)?.tree;
                let source = if renamed {
                    self.renamed_from(parent_tree, info.tree, &path)?
                } else if parent_tree == info.tree {
                    Some((path.clone(), file))
                } else {
                    tree_changes::file_at(self.repo, parent_tree, &path)?
                        .filter(|f| f.mode & 0o170000 == file.mode & 0o170000)
                        .map(|f| (path.clone(), f))
                };
                let Some((parent_path, parent_file)) = source else {
                    continue;
                };
                has_path[i] = true;
                let parent_origin = self.origin(parent, parent_path, parent_file);
                if parent_file.oid == file.oid {
                    // an unchanged file: every line came through this parent
                    let all = std::mem::take(&mut self.origins[at].pending);
                    return self.receive(parent_origin, all);
                }
                // a parent with the same content as an earlier one adds nothing
                let seen = takers[..i]
                    .iter()
                    .flatten()
                    .any(|&t| self.origins[t].file.oid == parent_file.oid);
                if !seen {
                    takers[i] = Some(parent_origin);
                }
            }
        }
        for parent_origin in takers.iter().flatten() {
            self.pass_unchanged(at, *parent_origin)?;
            if self.origins[at].pending.is_empty() {
                return Ok(());
            }
        }
        self.pass_moved(at, &info, &takers)
    }

    /// Moves to `parent` the pending lines of `at` that the diff between
    /// the two versions leaves unchanged. An entry that reaches across the
    /// edge of a change is cut there.
    fn pass_unchanged(&mut self, at: usize, parent: usize) -> Res<()> {
        let old = self.blob(self.origins[parent].file.oid)?;
        let new = self.blob(self.origins[at].file.oid)?;
        let edits = alt_diff::myers::diff_without_context(&old, &new);
        let mut kept = Vec::new();
        let mut passed = Vec::new();
        for mut e in std::mem::take(&mut self.origins[at].pending) {
            // two entries can cover the same lines here, reached through
            // different parents of a merge: each is placed on its own
            let mut next = edits.partition_point(|edit| edit.new.end <= e.at);
            while e.len > 0 {
                while next < edits.len() && edits[next].new.end <= e.at {
                    next += 1;
                }
                // the entry starts inside a change, or in the unchanged
                // lines in front of the next one
                let changed = next < edits.len() && edits[next].new.start <= e.at;
                let until = match (changed, edits.get(next)) {
                    (true, Some(edit)) => edit.new.end,
                    (false, Some(edit)) => edit.new.start,
                    (_, None) => usize::MAX,
                };
                let part = Entry {
                    len: e.len.min(until - e.at),
                    ..e
                };
                match changed {
                    true => kept.push(part),
                    false => passed.push(Entry {
                        at: before(&edits, next, part.at),
                        ..part
                    }),
                }
                e = Entry {
                    line: e.line + part.len,
                    at: e.at + part.len,
                    len: e.len - part.len,
                };
            }
        }
        self.origins[at].pending = kept;
        self.receive(parent, passed)
    }

    /// The file in the parent's tree that `path`, absent there, was
    /// renamed from in the commit's tree.
    fn renamed_from(
        &mut self,
        parent_tree: ObjectId,
        tree: ObjectId,
        path: &BString,
    ) -> Res<Option<(BString, TreeFile)>> {
        let changes = tree_changes::changes(self.repo, Some(parent_tree), Some(tree))?;
        // only the blamed path is looked for among the added files
        let wanted: Vec<_> = changes
            .into_iter()
            .filter(|c| c.new.is_none() || (c.old.is_none() && c.path == *path))
            .collect();
        Ok(tree_changes::pair_renames(self.repo, wanted)?
            .into_iter()
            .find(|d| d.path == *path)
            .and_then(|d| Some((d.old_path?, d.old?))))
    }
}

/// The line of the old side that the unchanged line `at` of the new side
/// corresponds to, `next` being the first edit that ends after it.
fn before(edits: &[Edit], next: usize, at: usize) -> usize {
    match next {
        0 => at,
        _ => at - edits[next - 1].new.end + edits[next - 1].old.end,
    }
}

/// Where each line starts, with the length of the content at the end.
pub(super) fn line_starts(content: &[u8]) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        content
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| i + 1),
    );
    if starts.last() != Some(&content.len()) {
        starts.push(content.len());
    }
    starts
}

/// The tree, parents and commit date of a commit, read off its headers.
fn commit_info(data: &[u8]) -> Option<CommitInfo> {
    let mut tree = None;
    let mut parents = Vec::new();
    let mut date = 0;
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(hex) = line.strip_prefix(b"tree ") {
            tree = ObjectId::from_hex(hex).ok();
        } else if let Some(hex) = line.strip_prefix(b"parent ") {
            parents.push(ObjectId::from_hex(hex).ok()?);
        } else if line.starts_with(b"committer ") {
            // `name <email> seconds zone`
            let seconds = line.rsplit(|&b| b == b' ').nth(1)?;
            date = std::str::from_utf8(seconds).ok()?.parse().unwrap_or(0);
        }
    }
    Some(CommitInfo {
        tree: tree?,
        parents,
        date,
    })
}
