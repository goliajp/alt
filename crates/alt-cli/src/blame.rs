//! `alt blame <path> [<rev>]`: for each line of a file, the commit that
//! introduced it.
//!
//! The walk is git's. Every line starts out pending at the starting commit.
//! A commit hands each pending line it did not change to the parent it took
//! the line from: a parent holding the identical file takes them all;
//! otherwise each parent in turn takes the lines its diff against the
//! commit leaves unchanged. Lines no parent takes belong to the commit.
//! Where a parent does not have the path, the file is followed across a
//! rename. Commits are visited newest first, so the lines a commit receives
//! from several children are settled together.
//!
//! Lines moved within a file or copied from other files (`-M` and `-C` in
//! git) are not traced.

use std::collections::{BinaryHeap, HashMap};
use std::io::Write;
use std::rc::Rc;

use alt_bytes::BString;
use alt_diff::split_lines;
use alt_git_codec::{Commit, ObjectId, ObjectKind};
use alt_repo::Repository;

use crate::tree_changes::{self, TreeFile};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// `alt blame <path> [<rev>]`. Writes one row per line to `out`:
/// `<short-oid> (<author-name> <YYYY-MM-DD> <line-no>) <line content>`
pub fn run(out: &mut impl Write, repo: &Repository, path: &str, rev: &str) -> Res<()> {
    let start = repo
        .rev_parse(rev)?
        .ok_or_else(|| format!("bad revision '{rev}'"))?;
    let (content, origins) = Board::new(repo).blame(start, path)?;
    let mut author_cache: HashMap<ObjectId, (String, String)> = HashMap::new();

    let pad_oid = 8;
    let mut pad_author = 0;
    for origin in &origins {
        let (name, _) = author_for(repo, *origin, &mut author_cache)?;
        pad_author = pad_author.max(name.len());
    }

    for (i, (line, origin)) in split_lines(&content).iter().zip(&origins).enumerate() {
        let (name, date) = author_for(repo, *origin, &mut author_cache)?;
        let short = origin.to_string();
        let short = &short[..pad_oid.min(short.len())];
        let mut text = String::from_utf8_lossy(line).into_owned();
        if text.ends_with('\n') {
            text.pop();
        }
        writeln!(
            out,
            "{short} ({name:<pad_author$} {date} {linenum:>4}) {text}",
            linenum = i + 1,
        )?;
    }
    Ok(())
}

/// A file as one commit has it.
struct Origin {
    commit: ObjectId,
    path: BString,
    file: TreeFile,
    /// lines still looking for their commit, as (line here, line in the
    /// blamed file), in order
    pending: Vec<(usize, usize)>,
}

struct CommitInfo {
    tree: ObjectId,
    parents: Vec<ObjectId>,
    date: i64,
}

struct Board<'r> {
    repo: &'r Repository,
    origins: Vec<Origin>,
    by_key: HashMap<(ObjectId, BString), usize>,
    by_commit: HashMap<ObjectId, Vec<usize>>,
    commits: HashMap<ObjectId, Rc<CommitInfo>>,
    /// commits with pending lines: newest first, then first queued
    queue: BinaryHeap<(i64, std::cmp::Reverse<u64>, ObjectId)>,
    queued: u64,
    blobs: HashMap<ObjectId, Rc<Vec<u8>>>,
}

impl<'r> Board<'r> {
    fn new(repo: &'r Repository) -> Self {
        Board {
            repo,
            origins: Vec::new(),
            by_key: HashMap::new(),
            by_commit: HashMap::new(),
            commits: HashMap::new(),
            queue: BinaryHeap::new(),
            queued: 0,
            blobs: HashMap::new(),
        }
    }

    /// The content of `path` at `start`, and the commit of each of its lines.
    fn blame(mut self, start: ObjectId, path: &str) -> Res<(Rc<Vec<u8>>, Vec<ObjectId>)> {
        let tree = self.commit(start)?.tree;
        let file = tree_changes::file_at(self.repo, tree, path.trim_matches('/').as_bytes())?
            .filter(|f| f.mode & 0o170000 != 0o160000)
            .ok_or_else(|| format!("path '{path}' not found in {start}"))?;
        let content = self.blob(file.oid)?;
        let lines = split_lines(&content).len();
        let first = self.origin(start, BString::from(path.trim_matches('/')), file);
        self.receive(first, (0..lines).map(|l| (l, l)).collect())?;

        let mut found = vec![start; lines];
        while let Some((_, _, commit)) = self.queue.pop() {
            // one commit can hold the file under several paths
            while let Some(at) = self.by_commit[&commit]
                .iter()
                .copied()
                .find(|&o| !self.origins[o].pending.is_empty())
            {
                self.pass_to_parents(at)?;
                for (_, line) in std::mem::take(&mut self.origins[at].pending) {
                    found[line] = commit;
                }
                self.blobs.remove(&self.origins[at].file.oid);
            }
        }
        Ok((content, found))
    }

    fn commit(&mut self, oid: ObjectId) -> Res<Rc<CommitInfo>> {
        if let Some(info) = self.commits.get(&oid) {
            return Ok(info.clone());
        }
        let commit = self.repo.read_commit(&oid)?;
        let info = Rc::new(CommitInfo {
            tree: commit
                .tree()
                .ok_or_else(|| format!("commit {oid} has no tree"))?,
            parents: commit.parents().collect(),
            date: commit.committer_date().unwrap_or(0),
        });
        self.commits.insert(oid, info.clone());
        Ok(info)
    }

    fn blob(&mut self, oid: ObjectId) -> Res<Rc<Vec<u8>>> {
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

    fn origin(&mut self, commit: ObjectId, path: BString, file: TreeFile) -> usize {
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
    fn receive(&mut self, at: usize, lines: Vec<(usize, usize)>) -> Res<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let origin = &mut self.origins[at];
        if !origin.pending.is_empty() {
            origin.pending.extend(lines);
            origin.pending.sort_unstable();
            return Ok(());
        }
        origin.pending = lines;
        let commit = origin.commit;
        let date = self.commit(commit)?.date;
        self.queue
            .push((date, std::cmp::Reverse(self.queued), commit));
        self.queued += 1;
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
                } else {
                    tree_changes::file_at(self.repo, parent_tree, path.as_slice())?
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
        for parent_origin in takers.into_iter().flatten() {
            self.pass_unchanged(at, parent_origin)?;
            if self.origins[at].pending.is_empty() {
                break;
            }
        }
        Ok(())
    }

    /// Moves to `parent` the pending lines of `at` that the diff between
    /// the two versions leaves unchanged.
    fn pass_unchanged(&mut self, at: usize, parent: usize) -> Res<()> {
        let old = self.blob(self.origins[parent].file.oid)?;
        let new = self.blob(self.origins[at].file.oid)?;
        let edits = alt_diff::myers::diff_without_context(&old, &new);
        let mut kept = Vec::new();
        let mut passed = Vec::new();
        let mut next = 0;
        for (line, blamed) in std::mem::take(&mut self.origins[at].pending) {
            while next < edits.len() && edits[next].new.end <= line {
                next += 1;
            }
            if next < edits.len() && edits[next].new.start <= line {
                kept.push((line, blamed));
                continue;
            }
            // unchanged: shifted by what the edits before it added or removed
            let before = match next {
                0 => line,
                _ => line - edits[next - 1].new.end + edits[next - 1].old.end,
            };
            passed.push((before, blamed));
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

fn author_for(
    repo: &Repository,
    commit_oid: ObjectId,
    cache: &mut HashMap<ObjectId, (String, String)>,
) -> Res<(String, String)> {
    if let Some(v) = cache.get(&commit_oid) {
        return Ok(v.clone());
    }
    let commit = repo.read_commit(&commit_oid)?;
    let parts = parse_author(&commit);
    cache.insert(commit_oid, parts.clone());
    Ok(parts)
}

/// Returns `(name, YYYY-MM-DD)`. Falls back to `"unknown"` when the
/// author header is unparseable; this is a display path, not a
/// correctness one.
fn parse_author(commit: &Commit) -> (String, String) {
    let Some(line) = commit.author() else {
        return ("unknown".to_string(), "0000-00-00".to_string());
    };
    let raw = line.to_vec();
    let s = String::from_utf8_lossy(&raw);
    // "Name <email> 1234567890 +0000"
    let mut name = "unknown".to_string();
    let mut date = "0000-00-00".to_string();
    if let Some(lt) = s.find('<') {
        name = s[..lt].trim().to_string();
        if let Some(close) = s[lt..].find('>') {
            let rest = s[lt + close + 1..].trim();
            if let Some(secs_str) = rest.split_whitespace().next()
                && let Ok(secs) = secs_str.parse::<i64>()
            {
                date = format_date_utc(secs);
            }
        }
    }
    (name, date)
}

fn format_date_utc(secs: i64) -> String {
    // Civil-from-days (Howard Hinnant) algorithm — UTC midnight of the
    // epoch-day containing `secs`. We only print the date, not the
    // time, so timezone offsets don't matter for display fidelity at
    // the per-commit grain.
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // 0..=146096
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}
