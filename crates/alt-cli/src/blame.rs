//! `alt blame <path> [<rev>]`: line-by-line origin attribution for one
//! file as of `<rev>` (default HEAD).
//!
//! Algorithm (classic git blame, first-parent only):
//!
//! 1. Read the file at `<rev>`, split it into lines, initially attribute
//!    every line to `<rev>` itself.
//! 2. Walk to the first parent. Read the same path there.
//!    - If the parent doesn't carry that path, stop: every still-unattributed
//!      line was introduced at `<rev>`.
//!    - If the parent's blob oid matches, the file was unchanged between
//!      this commit and its parent — rebase every still-unattributed
//!      line's origin to the parent and recurse.
//! 3. Otherwise run a line diff between parent and current content.
//!    - Lines that appear inside an Edit's `new` range were
//!      added/modified at this commit; they stay attributed to
//!      `current_commit` and stop carrying back.
//!    - Lines in the unchanged gaps move their attribution to the
//!      parent and are tracked at the parent's line indices for the
//!      next iteration.
//! 4. Stop when every line has reached a commit whose parent does not
//!    carry the line (origin is final), or when we reach a root commit.
//!
//! Out of scope (future work):
//! - Detection of cross-file copy / move (`-M` / `-C` in git blame).
//! - Walking merge parents non-trivially; we currently follow the first
//!   parent only.
//! - Author resolution lookups are O(history depth) — fine on small to
//!   medium files. A repo-scale blame should cache commit metadata.

use std::collections::HashMap;
use std::io::Write;

use alt_diff::{diff_lines, split_lines};
use alt_git_codec::{Commit, ObjectId, ObjectKind, Tree};
use alt_repo::Repository;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Clone)]
struct BlameLine {
    origin: ObjectId,
    text: Vec<u8>,
}

/// `alt blame <path> [<rev>]`. Writes one row per line to `out`:
/// `<short-oid> (<author-name> <YYYY-MM-DD> <line-no>) <line content>`
pub fn run(out: &mut impl Write, repo: &Repository, path: &str, rev: &str) -> Res<()> {
    let start_commit = repo
        .rev_parse(rev)?
        .ok_or_else(|| format!("bad revision '{rev}'"))?;
    let blamed = blame_file(repo, start_commit, path)?;
    let mut author_cache: HashMap<ObjectId, (String, String)> = HashMap::new();

    let pad_oid = 8;
    let mut pad_author = 0;
    for line in &blamed {
        let (name, _) = author_for(repo, line.origin, &mut author_cache)?;
        pad_author = pad_author.max(name.len());
    }

    for (i, line) in blamed.iter().enumerate() {
        let (name, date) = author_for(repo, line.origin, &mut author_cache)?;
        let short = line.origin.to_string();
        let short = &short[..pad_oid.min(short.len())];
        let mut text = String::from_utf8_lossy(&line.text).into_owned();
        if text.ends_with('\n') {
            text.pop();
        }
        writeln!(
            out,
            "{short} ({name:<pad_author$} {date} {linenum:>4}) {text}",
            short = short,
            name = name,
            date = date,
            linenum = i + 1,
            pad_author = pad_author,
            text = text,
        )?;
    }
    Ok(())
}

fn blame_file(repo: &Repository, start: ObjectId, path: &str) -> Res<Vec<BlameLine>> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err("empty path".into());
    }

    let mut current_commit = start;
    let mut current_blob = match resolve_path_blob(repo, current_commit, &segments)? {
        Some(o) => o,
        None => return Err(format!("path '{path}' not found in {start}").into()),
    };
    let mut current_content = read_blob_bytes(repo, current_blob)?;
    let mut blame_lines: Vec<BlameLine> = split_lines(&current_content)
        .iter()
        .map(|l| BlameLine {
            origin: current_commit,
            text: l.to_vec(),
        })
        .collect();
    let mut current_map: Vec<Option<usize>> = (0..blame_lines.len()).map(Some).collect();

    loop {
        // Stop if everything is finalised already.
        if current_map.iter().all(Option::is_none) {
            break;
        }
        let commit = repo.read_commit(&current_commit)?;
        let Some(parent_commit) = commit.parents().next() else {
            break;
        };
        let Some(parent_blob) = resolve_path_blob(repo, parent_commit, &segments)? else {
            // file was introduced at current_commit; remaining lines
            // stay attributed there
            break;
        };
        if parent_blob == current_blob {
            // No content change → just rebase to parent.
            for idx in current_map.iter().flatten() {
                blame_lines[*idx].origin = parent_commit;
            }
            current_commit = parent_commit;
            continue;
        }
        let parent_content = read_blob_bytes(repo, parent_blob)?;
        let parent_lines = split_lines(&parent_content);
        let current_lines = split_lines(&current_content);
        let edits = diff_lines(&parent_lines, &current_lines);

        let mut new_map: Vec<Option<usize>> = vec![None; parent_lines.len()];
        let mut cursor_old = 0usize;
        let mut cursor_new = 0usize;
        for edit in &edits {
            // unchanged gap before this edit
            while cursor_new < edit.new.start && cursor_old < edit.old.start {
                if let Some(blame_idx) = current_map[cursor_new] {
                    new_map[cursor_old] = Some(blame_idx);
                    blame_lines[blame_idx].origin = parent_commit;
                }
                cursor_new += 1;
                cursor_old += 1;
            }
            // edit.new lines: introduced/modified at current_commit;
            // pin them and drop their carry-back.
            for i in edit.new.clone() {
                if let Some(blame_idx) = current_map[i] {
                    blame_lines[blame_idx].origin = current_commit;
                }
            }
            cursor_old = edit.old.end;
            cursor_new = edit.new.end;
        }
        // trailing unchanged
        while cursor_new < current_lines.len() && cursor_old < parent_lines.len() {
            if let Some(blame_idx) = current_map[cursor_new] {
                new_map[cursor_old] = Some(blame_idx);
                blame_lines[blame_idx].origin = parent_commit;
            }
            cursor_new += 1;
            cursor_old += 1;
        }

        current_commit = parent_commit;
        current_blob = parent_blob;
        current_content = parent_content;
        current_map = new_map;
    }

    Ok(blame_lines)
}

fn resolve_path_blob(
    repo: &Repository,
    commit_oid: ObjectId,
    segments: &[&str],
) -> Res<Option<ObjectId>> {
    let commit = repo.read_commit(&commit_oid)?;
    let Some(mut current) = commit.tree() else {
        return Ok(None);
    };
    for (i, seg) in segments.iter().enumerate() {
        let tree = read_tree(repo, current)?;
        let Some(entry) = tree
            .entries
            .iter()
            .find(|e| String::from_utf8_lossy(e.name.as_slice()) == **seg)
        else {
            return Ok(None);
        };
        if i == segments.len() - 1 {
            // last segment: must be a blob
            if entry.mode.value() == 0o040000 {
                return Ok(None);
            }
            return Ok(Some(entry.oid));
        }
        if entry.mode.value() != 0o040000 {
            return Ok(None);
        }
        current = entry.oid;
    }
    Ok(None)
}

fn read_tree(repo: &Repository, oid: ObjectId) -> Res<Tree> {
    let obj = repo
        .read_object(&oid)?
        .ok_or_else(|| format!("tree {oid} missing"))?;
    if obj.kind != ObjectKind::Tree {
        return Err(format!("{oid} is not a tree").into());
    }
    Ok(Tree::parse(&obj.data, repo.algo())?)
}

fn read_blob_bytes(repo: &Repository, oid: ObjectId) -> Res<Vec<u8>> {
    let obj = repo
        .read_object(&oid)?
        .ok_or_else(|| format!("blob {oid} missing"))?;
    if obj.kind != ObjectKind::Blob {
        return Err(format!("{oid} is not a blob").into());
    }
    Ok(obj.data)
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
