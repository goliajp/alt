//! Myers line diff that makes the choices git's default diff makes, so the
//! same two files give the same changed lines as `git diff`.
//!
//! An edit script is not unique. To agree with git this follows the same
//! three steps: lines that cannot be part of a useful match are set aside
//! first; the rest goes through a divide-and-conquer middle-snake search
//! that gives up on optimality for large, expensive inputs; and each run of
//! changed lines is then slid to where it reads best (see `compact`).

use crate::Edit;
use crate::compact::compact;

/// One side of a diff.
pub(crate) struct Side<'a> {
    pub(crate) lines: &'a [&'a [u8]],
    /// equal lines share a class, across both sides
    pub(crate) class: Vec<u32>,
    /// which lines the diff reports as changed
    pub(crate) changed: Vec<bool>,
}

/// The side's lines that take part in the search: their positions in the
/// file, and their classes.
#[derive(Default)]
pub(crate) struct Kept {
    pub(crate) index: Vec<usize>,
    pub(crate) class: Vec<u32>,
}

/// a line with this many matches or more is too common to anchor on
const MAX_EQUAL_LIMIT: usize = 1024;
/// how far around a common line its neighbours are looked at
const SCAN_WINDOW: usize = 100;
const KEEP_RUN_FACTOR: usize = 4;

/// The changed regions between `a` and `b`, in increasing order.
pub fn diff_lines<'a>(a: &[&'a [u8]], b: &[&'a [u8]]) -> Vec<Edit> {
    let (mut one, mut two) = classify(a, b);
    search(&mut one, &mut two);
    compact(&mut one, &two);
    compact(&mut two, &one);
    script(&one.changed, &two.changed)
}

/// The changed regions between two files as git computes them for a diff
/// without context lines, which is what blame reads. A tail the files
/// share is dropped in whole 1024-byte blocks first; a change next to it
/// can then not slide into it.
pub fn diff_without_context(old: &[u8], new: &[u8]) -> Vec<Edit> {
    const BLOCK: usize = 1024;
    let mut trimmed = 0;
    while trimmed + BLOCK <= old.len().min(new.len())
        && old[old.len() - trimmed - BLOCK..old.len() - trimmed]
            == new[new.len() - trimmed - BLOCK..new.len() - trimmed]
    {
        trimmed += BLOCK;
    }
    // the cut moves forward to the end of the line it falls in
    let tail = &old[old.len() - trimmed..];
    let recovered = tail
        .iter()
        .position(|&b| b == b'\n')
        .map_or(trimmed, |at| at + 1);
    let cut = trimmed - recovered;
    let a = crate::split_lines(&old[..old.len() - cut]);
    let b = crate::split_lines(&new[..new.len() - cut]);
    diff_lines(&a, &b)
}

fn classify<'a>(a: &'a [&'a [u8]], b: &'a [&'a [u8]]) -> (Side<'a>, Side<'a>) {
    let mut classes: std::collections::HashMap<&[u8], u32> = std::collections::HashMap::new();
    let mut class_of = |line: &'a [u8]| {
        let next = classes.len() as u32;
        *classes.entry(line).or_insert(next)
    };
    let side = |lines: &'a [&'a [u8]], class: Vec<u32>| Side {
        lines,
        class,
        changed: vec![false; lines.len()],
    };
    let class_a = a.iter().map(|l| class_of(l)).collect();
    let class_b = b.iter().map(|l| class_of(l)).collect();
    (side(a, class_a), side(b, class_b))
}

fn search(one: &mut Side<'_>, two: &mut Side<'_>) {
    let (n1, n2) = (one.class.len(), two.class.len());
    // lines both files begin and end with are not part of the problem
    let head = (0..n1.min(n2))
        .take_while(|&i| one.class[i] == two.class[i])
        .count();
    let tail = (0..n1.min(n2) - head)
        .take_while(|&i| one.class[n1 - 1 - i] == two.class[n2 - 1 - i])
        .count();

    let classes = one
        .class
        .iter()
        .chain(&two.class)
        .max()
        .map_or(0, |m| *m as usize + 1);
    let mut count1 = vec![0usize; classes];
    let mut count2 = vec![0usize; classes];
    one.class.iter().for_each(|&c| count1[c as usize] += 1);
    two.class.iter().for_each(|&c| count2[c as usize] += 1);

    let kept1 = set_aside(one, head, n1 - tail, &count2);
    let kept2 = set_aside(two, head, n2 - tail, &count1);

    crate::middle::search(&kept1, &kept2, &mut one.changed, &mut two.changed);
}

/// Marks as changed the lines of `side[from..to]` that are better left out
/// of the search, and returns the rest: a line with no equal on the other
/// side can only be a change, and a line with very many equals, sitting
/// among lines that have none, would only produce chance matches.
fn set_aside(side: &mut Side<'_>, from: usize, to: usize, other_count: &[usize]) -> Kept {
    let limit = crate::middle::rough_sqrt(side.class.len()).min(MAX_EQUAL_LIMIT);
    // 0: no equal on the other side, 1: some, 2: too many
    let grade: Vec<u8> = (from..to)
        .map(|i| match other_count[side.class[i] as usize] {
            0 => 0,
            n if n >= limit => 2,
            _ => 1,
        })
        .collect();
    let mut kept = Kept::default();
    for i in from..to {
        let at = i - from;
        if grade[at] == 1 || (grade[at] == 2 && !lost_among_unmatched(&grade, at)) {
            kept.index.push(i);
            kept.class.push(side.class[i]);
        } else {
            side.changed[i] = true;
        }
    }
    kept
}

/// Whether the too-common line `grade[at]` sits in a run of lines that are
/// unmatched or too common, with unmatched lines on both sides of it and
/// enough of them overall.
fn lost_among_unmatched(grade: &[u8], at: usize) -> bool {
    let start = at.saturating_sub(SCAN_WINDOW);
    let end = (at + SCAN_WINDOW).min(grade.len() - 1);
    let run = |positions: &mut dyn Iterator<Item = usize>| {
        let (mut unmatched, mut common) = (0, 0);
        for i in positions {
            match grade[i] {
                0 => unmatched += 1,
                2 => common += 1,
                _ => break,
            }
        }
        (unmatched, common)
    };
    let (unmatched_before, common_before) = run(&mut (start..at).rev());
    if unmatched_before == 0 {
        return false;
    }
    let (unmatched_after, common_after) = run(&mut (at + 1..=end));
    if unmatched_after == 0 {
        return false;
    }
    let unmatched = unmatched_before + unmatched_after;
    // the line itself counts on each side
    let common = common_before + common_after + 2;
    common * KEEP_RUN_FACTOR < common + unmatched
}

/// Turns the two changed-line maps into edits: each maximal stretch where
/// either side has changed lines is one edit.
fn script(changed1: &[bool], changed2: &[bool]) -> Vec<Edit> {
    let mut edits = Vec::new();
    let (mut i1, mut i2) = (0, 0);
    while i1 < changed1.len() || i2 < changed2.len() {
        let (start1, start2) = (i1, i2);
        while i1 < changed1.len() && changed1[i1] {
            i1 += 1;
        }
        while i2 < changed2.len() && changed2[i2] {
            i2 += 1;
        }
        if i1 > start1 || i2 > start2 {
            edits.push(Edit {
                old: start1..i1,
                new: start2..i2,
            });
        } else {
            // an unchanged line, present on both sides
            i1 += 1;
            i2 += 1;
        }
    }
    edits
}
