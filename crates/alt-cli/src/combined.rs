//! The combined diff of a merge commit, git's `--cc`: for each file the
//! merge left different from every parent, the result with one column per
//! parent saying what it added to that parent and which of the parent's
//! lines it dropped. Hunks where the merge only took one side over the
//! others are left out; what remains is what the merge itself decided.

use std::collections::BTreeMap;
use std::io::Write;

use alt_bytes::BString;
use alt_git_codec::ObjectId;
use alt_repo::Repository;

use crate::file_header::Style;
use crate::tree_changes::{self, TreeFile};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// What a parent and the result hold at a path.
type Sides = (Option<TreeFile>, Option<TreeFile>);

/// lines of context around a change
const CONTEXT: usize = 3;

/// A line some parents had where the result has none.
struct Lost {
    text: Vec<u8>,
    /// bit `n`: parent `n` had this line
    parents: u64,
}

/// One line of the result, with what was lost in front of it. There is
/// one more than the result has lines, for what was lost at the end.
#[derive(Default)]
struct Line {
    lost: Vec<Lost>,
    /// lines of the parent being compared, not yet merged into `lost`
    pending: Vec<Vec<u8>>,
    /// bit `n`: the line is not in parent `n`; above the parent bits, the
    /// marks of the hunk layout
    flag: u64,
    /// per parent, the number of its line that corresponds to this one
    parent_line: Vec<usize>,
}

/// Writes the combined diff of the commit with tree `result` against the
/// trees of its `parents`.
pub(crate) fn write(
    out: &mut impl Write,
    repo: &Repository,
    result: ObjectId,
    parents: &[Option<ObjectId>],
    style: &Style<'_>,
) -> Res<()> {
    // per path, what each parent had there, for the paths that differ
    // from that parent
    let mut sides: Vec<BTreeMap<BString, Sides>> = Vec::new();
    for parent in parents {
        let changes = tree_changes::changes(repo, *parent, Some(result))?;
        let files = tree_changes::pair_renames(repo, changes)?;
        sides.push(
            files
                .into_iter()
                .map(|f| (f.path, (f.old, f.new)))
                .collect(),
        );
    }
    let Some((first, rest)) = sides.split_first() else {
        return Ok(());
    };
    for (path, (_, new)) in first {
        // only a path that differs from every parent is the merge's doing
        let Some(olds) = sides
            .iter()
            .map(|side| side.get(path).map(|(old, _)| *old))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        debug_assert!(rest.iter().all(|side| side[path].1 == *new));
        write_file(out, repo, path, &olds, *new, style)?;
    }
    Ok(())
}

fn content(repo: &Repository, side: Option<TreeFile>) -> Res<Vec<u8>> {
    match side {
        None => Ok(Vec::new()),
        Some(f) if f.mode == 0o160000 => Ok(format!("Subproject commit {}\n", f.oid).into_bytes()),
        Some(f) => Ok(repo
            .read_object(&f.oid)?
            .ok_or_else(|| format!("blob {} missing", f.oid))?
            .data),
    }
}

/// Lines without their newline; a last line that lacks one still counts.
fn lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    if data.is_empty() || data.ends_with(b"\n") {
        out.pop();
    }
    out
}

fn write_file(
    out: &mut impl Write,
    repo: &Repository,
    path: &BString,
    olds: &[Option<TreeFile>],
    new: Option<TreeFile>,
    style: &Style<'_>,
) -> Res<()> {
    let n = olds.len();
    let result = content(repo, new)?;
    let result_lines = lines(&result);
    let count = result_lines.len();
    let mut table: Vec<Line> = (0..count + 2)
        .map(|_| Line {
            parent_line: vec![0; n],
            ..Line::default()
        })
        .collect();
    let mut binary = alt_diff::is_binary(&result);
    for (i, old) in olds.iter().enumerate() {
        // a parent with the same content as an earlier one reads the same
        if let Some(j) = (0..i).find(|&j| olds[j].map(|f| f.oid) == old.map(|f| f.oid)) {
            reuse(&mut table, count, i, j);
            continue;
        }
        let parent = content(repo, *old)?;
        binary |= alt_diff::is_binary(&parent);
        compare(&mut table, count, i, &parent, &result);
    }
    let mode = |f: Option<TreeFile>| f.map_or(0, |f| f.mode);
    let mode_differs = olds.iter().any(|old| mode(*old) != mode(new));
    if binary {
        write_header(out, path, olds, new, mode_differs, false, style)?;
        out.write_all(b"Binary files differ\n")?;
        return Ok(());
    }
    if arrange(&mut table, count, n) || mode_differs {
        write_header(out, path, olds, new, mode_differs, true, style)?;
        write_hunks(out, &table, &result_lines, n)?;
    }
    Ok(())
}

/// Records how the result differs from parent `n`: which result lines the
/// parent lacks, and which parent lines are gone and where.
fn compare(table: &mut [Line], count: usize, n: usize, parent: &[u8], result: &[u8]) {
    let bit = 1u64 << n;
    let parent_lines = lines(parent);
    for edit in alt_diff::myers::diff_without_context(parent, result) {
        // lines the parent lost hang in front of the result line that
        // follows them; for a replacement, the first replacing line
        let bucket = edit.new.start;
        for line in &parent_lines[edit.old.clone()] {
            table[bucket].pending.push(line.to_vec());
        }
        for at in edit.new.clone() {
            table[at].flag |= bit;
        }
    }
    let mut parent_line = 1;
    for (at, line) in table.iter_mut().enumerate().take(count + 1) {
        line.parent_line[n] = parent_line;
        merge_lost(&mut line.lost, std::mem::take(&mut line.pending), bit);
        parent_line += line.lost.iter().filter(|l| l.parents & bit != 0).count();
        if at < count && line.flag & bit == 0 {
            parent_line += 1;
        }
    }
    table[count + 1].parent_line[n] = parent_line;
}

/// Adds the lines one more parent lost at a spot to those already there.
/// A line the earlier parents lost too, in the same order, is the same
/// line: the two lists are merged along their longest common subsequence.
fn merge_lost(base: &mut Vec<Lost>, new: Vec<Vec<u8>>, bit: u64) {
    if new.is_empty() {
        return;
    }
    let (rows, cols) = (base.len(), new.len());
    #[derive(Clone, Copy, PartialEq)]
    enum Step {
        Match,
        Base,
        New,
    }
    let mut length = vec![vec![0usize; cols + 1]; rows + 1];
    let mut step = vec![vec![Step::Base; cols + 1]; rows + 1];
    step[0][1..].fill(Step::New);
    for i in 1..=rows {
        for j in 1..=cols {
            (length[i][j], step[i][j]) = if base[i - 1].text == new[j - 1] {
                (length[i - 1][j - 1] + 1, Step::Match)
            } else if length[i][j - 1] >= length[i - 1][j] {
                (length[i][j - 1], Step::New)
            } else {
                (length[i - 1][j], Step::Base)
            };
        }
    }
    let mut new: Vec<Option<Vec<u8>>> = new.into_iter().map(Some).collect();
    let (mut i, mut j) = (rows, cols);
    while i != 0 || j != 0 {
        match step[i][j] {
            Step::Match => {
                base[i - 1].parents |= bit;
                i -= 1;
                j -= 1;
            }
            Step::New => {
                let text = new[j - 1].take().expect("each new line is placed once");
                base.insert(i, Lost { text, parents: bit });
                j -= 1;
            }
            Step::Base => i -= 1,
        }
    }
}

/// Parent `i` holds the same content as parent `j`: it differs from the
/// result exactly as `j` does.
fn reuse(table: &mut [Line], count: usize, i: usize, j: usize) {
    let (ibit, jbit) = (1u64 << i, 1u64 << j);
    for line in table.iter_mut().take(count + 2) {
        line.parent_line[i] = line.parent_line[j];
        for lost in line.lost.iter_mut().filter(|l| l.parents & jbit != 0) {
            lost.parents |= ibit;
        }
        if line.flag & jbit != 0 {
            line.flag |= ibit;
        }
    }
}

/// Decides which lines are printed: marks the changed lines that are the
/// merge's own doing and the context around them. Returns whether there
/// is anything to print.
fn arrange(table: &mut [Line], count: usize, n: usize) -> bool {
    let all = (1u64 << n) - 1;
    let mark = 1u64 << n;
    for line in table.iter_mut().take(count + 1) {
        match line.flag & all != 0 || !line.lost.is_empty() {
            true => line.flag |= mark,
            false => line.flag &= !mark,
        }
    }
    // a stretch of changes that all come from taking the same parents'
    // version is no decision of the merge, unless it differs from all
    let mut i = 0;
    while i <= count {
        while i <= count && table[i].flag & mark == 0 {
            i += 1;
        }
        if count < i {
            break;
        }
        let begin = i;
        let mut j = i + 1;
        while j <= count {
            if table[j].flag & mark == 0 {
                // does another change follow within reach of the context?
                let tail = hunk_tail(table, all, begin, j);
                let mut ahead = (tail + CONTEXT).min(count + 1);
                let mut joined = false;
                while j < ahead {
                    ahead -= 1;
                    if table[ahead].flag & mark != 0 {
                        joined = true;
                        break;
                    }
                }
                if !joined {
                    break;
                }
                j = ahead;
            }
            j += 1;
        }
        let end = j;
        let mut same = 0u64;
        let mut own = false;
        'stretch: for line in &table[begin..end] {
            let from = [line.flag & all].into_iter().filter(|d| *d != 0);
            for differs in from.chain(line.lost.iter().map(|l| l.parents)) {
                if same == 0 {
                    same = differs;
                } else if same != differs {
                    own = true;
                    break 'stretch;
                }
            }
        }
        if !own && same != all {
            for line in &mut table[begin..end] {
                line.flag &= !mark;
            }
        }
        i = end;
    }
    give_context(table, count, n)
}

/// `at` is the first unmarked line after a hunk. When the hunk's last line
/// is marked only for what was lost in front of it, the line itself reads
/// as context already, and the tail starts one line earlier.
fn hunk_tail(table: &[Line], all: u64, begin: usize, at: usize) -> usize {
    match begin < at && table[at - 1].flag & all == 0 {
        true => at - 1,
        false => at,
    }
}

/// The first line from `from` on that is marked, or that is unmarked.
fn next(table: &[Line], mark: u64, from: usize, count: usize, marked: bool) -> usize {
    (from..=count)
        .find(|&i| (table[i].flag & mark != 0) == marked)
        .unwrap_or(count + 1)
}

/// Marks the context lines around the marked lines, joining hunks whose
/// contexts would touch.
fn give_context(table: &mut [Line], count: usize, n: usize) -> bool {
    let all = (1u64 << n) - 1;
    let mark = 1u64 << n;
    let no_lost_before = 2u64 << n;
    let mut i = next(table, mark, 0, count, true);
    if count < i {
        return false;
    }
    while i <= count {
        // the lines leading up to the hunk; what was lost in front of
        // them belongs to no hunk and is not shown
        for line in &mut table[i.saturating_sub(CONTEXT)..i] {
            if line.flag & mark == 0 {
                line.flag |= no_lost_before;
            }
            line.flag |= mark;
        }
        loop {
            let mut j = next(table, mark, i, count, false);
            if count < j {
                return true; // marked to the end
            }
            let k = next(table, mark, j, count, true);
            j = hunk_tail(table, all, i, j);
            if k < j + CONTEXT {
                // the gap to the next hunk is small: one hunk
                for line in &mut table[j..k] {
                    line.flag |= mark;
                }
                i = k;
                continue;
            }
            i = k;
            for line in &mut table[j..(j + CONTEXT).min(count + 1)] {
                line.flag |= mark;
            }
            break;
        }
    }
    true
}

fn write_header(
    out: &mut impl Write,
    path: &BString,
    olds: &[Option<TreeFile>],
    new: Option<TreeFile>,
    mode_differs: bool,
    with_names: bool,
    style: &Style<'_>,
) -> Res<()> {
    out.write_all(b"diff --cc ")?;
    out.write_all(&style.name("", path))?;
    out.write_all(b"\nindex ")?;
    let ids: Vec<String> = olds
        .iter()
        .map(|old| style.short(old.map(|f| f.oid)))
        .collect();
    writeln!(
        out,
        "{}..{}",
        ids.join(","),
        style.short(new.map(|f| f.oid))
    )?;
    let added = new.is_some() && olds.iter().all(Option::is_none);
    if mode_differs {
        match (added, new) {
            (true, Some(f)) => writeln!(out, "new file mode {:06o}", f.mode)?,
            _ => {
                let modes: Vec<String> = olds
                    .iter()
                    .map(|old| format!("{:06o}", old.map_or(0, |f| f.mode)))
                    .collect();
                let deleted = if new.is_none() { "deleted file " } else { "" };
                let result = new.map_or(String::new(), |f| format!("..{:06o}", f.mode));
                writeln!(out, "{deleted}mode {}{result}", modes.join(","))?;
            }
        }
    }
    if !with_names {
        return Ok(());
    }
    let name = |exists: bool, prefix| match exists {
        true => style.name(prefix, path),
        false => b"/dev/null".to_vec(),
    };
    out.write_all(b"--- ")?;
    out.write_all(&name(!added, "a/"))?;
    out.write_all(b"\n+++ ")?;
    out.write_all(&name(new.is_some(), "b/"))?;
    out.write_all(b"\n")?;
    Ok(())
}

/// A line that names the section a hunk is in: one starting with a
/// letter, `_` or `$`.
fn names_section(line: &[u8]) -> bool {
    line.first()
        .is_some_and(|&c| c.is_ascii_alphabetic() || c == b'_' || c == b'$')
}

fn write_line(out: &mut impl Write, text: &[u8]) -> std::io::Result<()> {
    out.write_all(text)?;
    out.write_all(b"\n")
}

fn write_hunks(out: &mut impl Write, table: &[Line], result: &[&[u8]], n: usize) -> Res<()> {
    let count = result.len();
    let mark = 1u64 << n;
    let no_lost_before = 2u64 << n;
    let mut at = 0;
    loop {
        let mut section: Option<&[u8]> = None;
        while at <= count && table[at].flag & mark == 0 {
            if at < count && names_section(result[at]) {
                section = Some(result[at]);
            }
            at += 1;
        }
        if count < at {
            return Ok(());
        }
        let end = (at + 1..=count)
            .find(|&i| table[i].flag & mark == 0)
            .unwrap_or(count + 1);
        // the entry past the last line only carries what was lost there
        let shown = end - at - usize::from(count < end);
        out.write_all("@".repeat(n + 1).as_bytes())?;
        for parent in 0..n {
            let (from, to) = (
                table[at].parent_line[parent],
                table[end].parent_line[parent],
            );
            write!(out, " -{from},{}", to - from)?;
        }
        write!(out, " +{},{shown} {}", at + 1, "@".repeat(n + 1))?;
        if let Some(text) = section {
            // git shows the first 40 bytes, up to but leaving out the
            // last one that is not a space
            let last = (0..text.len().min(40)).rfind(|&i| !text[i].is_ascii_whitespace());
            if let Some(last) = last.filter(|&l| l > 0) {
                out.write_all(b" ")?;
                out.write_all(&text[..last])?;
            }
        }
        out.write_all(b"\n")?;
        while at < end {
            let line = &table[at];
            at += 1;
            if line.flag & no_lost_before == 0 {
                for lost in &line.lost {
                    let columns: String = (0..n)
                        .map(|p| if lost.parents >> p & 1 != 0 { '-' } else { ' ' })
                        .collect();
                    out.write_all(columns.as_bytes())?;
                    write_line(out, &lost.text)?;
                }
            }
            if count < at {
                break;
            }
            let columns: String = (0..n)
                .map(|p| if line.flag >> p & 1 != 0 { '+' } else { ' ' })
                .collect();
            out.write_all(columns.as_bytes())?;
            write_line(out, result[at - 1])?;
        }
    }
}
