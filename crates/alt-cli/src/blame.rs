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
//! With `-M` and `-C`, lines a commit moved within the file or copied
//! from another file are traced on through where they came from.

mod moved;
mod walk;

use std::collections::HashMap;
use std::io::Write;

use alt_diff::split_lines;
use alt_git_codec::{Commit, ObjectId};
use alt_repo::Repository;

use walk::Board;
pub use walk::Options;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// `alt blame <path> [<rev>]`, the revision `rev` given as `named`. Writes
/// one row per line to `out`:
/// `<short-oid> (<author-name> <YYYY-MM-DD> <line-no>) <line content>`
pub fn run(
    out: &mut impl Write,
    repo: &Repository,
    path: &str,
    rev: &str,
    named: &str,
    options: Options,
) -> Res<()> {
    let start = repo
        .rev_parse(rev)?
        .ok_or_else(|| format!("bad revision '{rev}'"))?;
    let (content, origins) = Board::new(repo, options).blame(start, path, named)?;
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
