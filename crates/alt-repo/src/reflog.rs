//! The log of a ref: the values it was given, one after another, and
//! what was done each time.
//!
//! A git repository keeps it in a file for each ref under `logs/`. A
//! native store has the same of every ref in its operations; what git
//! had logged before an import is kept beside the store as it was, and
//! the operations since then go on from there. The stack of stashed
//! entries, which git reads from the log of `refs/stash`, is told from
//! the ref the store has for each entry.

mod again;
mod asked;
mod derived;
mod said;

use std::fs;
use std::path::{Path, PathBuf};

use alt_git_codec::ObjectId;
use alt_git_refs::reflog::{Line, parse};
use alt_git_refs::stash;
use alt_oplog::OpId;

use crate::{Backend, RepoError, Repository};

pub use again::{Carried, carried, was_at};

/// Where an import keeps the logs of the git repository, from the store.
const KEPT: &str = "git-import/logs";
/// The file that names the last op of the store when they were kept.
const KEPT_AT: &str = "git-import/logs.at";

/// One line of the log of a ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
    /// `name <email> seconds zone` where git logged the line
    pub by: Option<Vec<u8>>,
    /// when the operation was recorded, for a line read from one
    pub at_ms: Option<u64>,
    /// what was done; `None` for a line git logged without it
    pub said: Option<Vec<u8>>,
}

/// What was done at a move: as git logged it, or what the operation
/// tells of it.
enum Said {
    Kept(Option<Vec<u8>>),
    Of {
        verb: String,
        /// the refs the head was on before and after, where that changed
        from: Option<String>,
        to: Option<String>,
        /// the new base of the rebase the move ends
        onto: Option<ObjectId>,
        /// for a merge, the one branch that was at the commit moved to
        tip: Option<String>,
    },
}

struct Move {
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    by: Option<Vec<u8>>,
    at_ms: Option<u64>,
    said: Said,
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, RepoError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound) => Ok(None),
        // a name with a ref beneath it is a directory there
        Err(_) if path.is_dir() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The lines of the log file of `name` under `logs`, none without one.
fn logged(logs: &Path, name: &str) -> Result<Vec<Move>, RepoError> {
    let plain = name.split('/').all(|part| !matches!(part, "" | "." | ".."));
    let Some(log) = read(&logs.join(name))?.filter(|_| plain) else {
        return Ok(Vec::new());
    };
    let kept = |line: Line| Move {
        old: line.old,
        new: line.new,
        by: Some(line.by),
        at_ms: None,
        said: Said::Kept(line.said),
    };
    Ok(parse(&log).into_iter().map(kept).collect())
}

fn op_id(hex: &str) -> Option<OpId> {
    let mut id = [0u8; 32];
    let hex = hex.trim().as_bytes();
    if hex.len() != 64 {
        return None;
    }
    for (byte, pair) in id.iter_mut().zip(hex.chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(OpId(id))
}

/// Whether `name` is the head of a workspace.
fn is_head(name: &str) -> bool {
    name == "HEAD" || (name.starts_with("workspaces/") && name.ends_with("/HEAD"))
}

/// Keeps the logs git has of the refs of the git directory `dir` in the
/// store under `alt_dir`, in place of what an earlier import kept, and
/// notes that the log of the store ended at the op `last` names then: the moves of a
/// ref are those git logged and then those of the operations after it.
/// The log of `refs/stash` is the stack, which the import reads itself.
/// `ours` are the logs the store had before this import: one that git's
/// does not begin with stays, and ends with the import.
pub fn take(
    dir: &Path,
    alt_dir: &Path,
    last: Option<String>,
    ours: (Carried, alt_git_codec::HashAlgo),
) -> Result<(), RepoError> {
    let kept = alt_dir.join(KEPT);
    if kept.exists() {
        fs::remove_dir_all(&kept)?;
    }
    let mut todo = vec![PathBuf::new()];
    while let Some(under) = todo.pop() {
        let from = dir.join("logs").join(&under);
        let Ok(listed) = fs::read_dir(&from) else {
            continue;
        };
        for entry in listed {
            let entry = entry?;
            let path = under.join(entry.file_name());
            if path == Path::new(stash::TOP) {
                continue;
            }
            if entry.file_type()?.is_dir() {
                todo.push(path);
                continue;
            }
            let to = kept.join(&path);
            fs::create_dir_all(to.parent().expect("a file under the directory"))?;
            fs::copy(entry.path(), to)?;
        }
    }
    again::keep_ours(alt_dir, ours.0, ours.1)?;
    fs::create_dir_all(alt_dir.join("git-import"))?;
    Ok(fs::write(alt_dir.join(KEPT_AT), last.unwrap_or_default())?)
}

impl Repository {
    fn moves(&self, name: &str) -> Result<Vec<Move>, RepoError> {
        let alt = match &self.backend {
            Backend::Git { .. } => return logged(&self.repo_dir.join("logs"), name),
            Backend::Alt(alt) => alt,
        };
        if name == stash::TOP {
            return self.stashed();
        }
        // what an import kept ends where the log of the store was then
        let after =
            read(&alt.alt_dir.join(KEPT_AT))?.map(|at| op_id(&String::from_utf8_lossy(&at)));
        let since = derived::derived(alt.refs()?, name, is_head(name), after)?;
        let mut moves = match since.anew {
            true => Vec::new(),
            false => logged(&alt.alt_dir.join(KEPT), name)?,
        };
        moves.extend(since.moves);
        Ok(moves)
    }

    /// The stack of stashed entries as the log of `refs/stash` has it.
    fn stashed(&self) -> Result<Vec<Move>, RepoError> {
        let refs = self.list_refs()?;
        let entry = |(name, oid, _): &(String, ObjectId, Option<String>)| {
            Some((stash::entry_number(name)?, *oid))
        };
        let mut entries: Vec<(u64, ObjectId)> = refs.iter().filter_map(entry).collect();
        entries.sort();
        let mut moves = Vec::new();
        let mut before = None;
        for (_, commit) in entries {
            let parsed = self.read_commit(&commit)?;
            let subject = parsed.message().split(|b| *b == b'\n').next();
            moves.push(Move {
                old: before.replace(commit),
                new: Some(commit),
                by: parsed.committer().map(|by| by.to_vec()),
                at_ms: None,
                said: Said::Kept(Some(subject.unwrap_or_default().to_vec())),
            });
        }
        Ok(moves)
    }

    /// The log of the ref `name`, a full name, the oldest line first;
    /// empty where the ref has none.
    pub fn reflog(&self, name: &str) -> Result<Vec<Entry>, RepoError> {
        let mut entries = Vec::new();
        for moved in self.moves(name)? {
            let said = match &moved.said {
                Said::Kept(said) => said.clone(),
                Said::Of { .. } => Some(self.tell(name, &moved)?),
            };
            entries.push(Entry {
                old: moved.old,
                new: moved.new,
                by: moved.by,
                at_ms: moved.at_ms,
                said,
            });
        }
        Ok(entries)
    }

    /// What `name` named `back` moves ago: what it names now for none.
    /// `Err` with the number of moves the log has where it has fewer.
    pub fn logged_at(&self, name: &str, back: usize) -> Result<Result<ObjectId, usize>, RepoError> {
        let moves = self.moves(name)?;
        let found = match moves.len().checked_sub(back) {
            Some(0) => moves.first().and_then(|first| first.old),
            Some(at) => moves[at - 1].new,
            None => None,
        };
        Ok(found.ok_or(moves.len()))
    }

    /// The branch the head ref `head` was on before it last went from
    /// one to another, as the log tells it, or `nth` such moves before
    /// that: its short name, or what git logged in its place.
    pub fn branch_before(&self, head: &str, nth: usize) -> Result<Option<String>, RepoError> {
        let short = |name: &str| name.strip_prefix("refs/heads/").map(str::to_owned);
        let left = |moved: &Move| match &moved.said {
            Said::Kept(Some(said)) => {
                let said = std::str::from_utf8(said).ok()?;
                let (from, _) = said
                    .strip_prefix("checkout: moving from ")?
                    .split_once(' ')?;
                Some(from.to_owned())
            }
            Said::Of {
                from: Some(from),
                to: Some(to),
                ..
            } => short(from).filter(|_| to.starts_with("refs/heads/")),
            _ => None,
        };
        let moves = self.moves(head)?;
        Ok(moves.iter().rev().filter_map(left).nth(nth))
    }
}
