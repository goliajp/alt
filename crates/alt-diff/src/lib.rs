//! Line-level text diff using the **histogram** algorithm — git's default
//! since 2.x, a refinement of patience diff that anchors on rare lines for
//! cleaner hunks than Myers. Plus unified-diff (`@@ … @@`) formatting and
//! binary detection.
//!
//! Stone: no business types, no I/O. Bytes in, structured edits / unified
//! text out. Correctness invariant (fuzzed): applying the returned edits to
//! `old` reproduces `new`, for any input.

pub mod binary;
mod histogram;

use std::ops::Range;

/// One changed region: the lines `old[old.clone()]` were replaced by
/// `new[new.clone()]`. A pure insertion has an empty `old`; a pure deletion
/// has an empty `new`. Ranges are line indices (0-based, half-open).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub old: Range<usize>,
    pub new: Range<usize>,
}

/// Splits `data` into lines, each including its trailing `\n` (the last line
/// has none if `data` does not end in `\n`). An empty input is zero lines.
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            lines.push(&data[start..=i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    lines
}

/// True if `data` looks binary: git's heuristic is a NUL byte in the first
/// 8000 bytes.
pub fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

/// Histogram line diff of two byte buffers. Returns the changed regions in
/// increasing line order; equal regions are the gaps between them.
pub fn diff(old: &[u8], new: &[u8]) -> Vec<Edit> {
    let a = split_lines(old);
    let b = split_lines(new);
    diff_lines(&a, &b)
}

/// Histogram diff over pre-split line slices. Both sides share a lifetime so
/// their lines can be interned into one table.
pub fn diff_lines<'a>(a: &[&'a [u8]], b: &[&'a [u8]]) -> Vec<Edit> {
    // intern lines to integer tokens so the core compares u32s, not bytes
    let mut interner: std::collections::HashMap<&'a [u8], u32> = std::collections::HashMap::new();
    let mut intern = |line: &'a [u8]| -> u32 {
        let next = interner.len() as u32;
        *interner.entry(line).or_insert(next)
    };
    let at: Vec<u32> = a.iter().map(|&l| intern(l)).collect();
    let bt: Vec<u32> = b.iter().map(|&l| intern(l)).collect();

    let mut edits = Vec::new();
    histogram::diff(&at, 0..at.len(), &bt, 0..bt.len(), &mut edits);
    coalesce(edits)
}

/// Merges edits that touch (adjacent or overlapping) so callers see one
/// region per contiguous change. The histogram recursion already yields
/// disjoint, ordered edits; adjacent ones are still worth merging for hunk
/// formatting.
fn coalesce(edits: Vec<Edit>) -> Vec<Edit> {
    let mut out: Vec<Edit> = Vec::with_capacity(edits.len());
    for e in edits {
        if e.old.is_empty() && e.new.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.old.end >= e.old.start && last.new.end >= e.new.start => {
                last.old.end = e.old.end.max(last.old.end);
                last.new.end = e.new.end.max(last.new.end);
            }
            _ => out.push(e),
        }
    }
    out
}

/// A unified-diff hunk: a window of old/new lines with its `@@` header
/// coordinates (1-based, git convention) already computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    /// `(tag, line)` where tag is b' ' (context), b'-' (removed), b'+' (added).
    pub lines: Vec<(u8, Vec<u8>)>,
}

/// Groups edits into unified-diff hunks with `context` lines of surrounding
/// context, merging hunks whose contexts touch (git's behaviour).
pub fn hunks(old: &[u8], new: &[u8], context: usize) -> Vec<Hunk> {
    let a = split_lines(old);
    let b = split_lines(new);
    let edits = diff_lines(&a, &b);
    if edits.is_empty() {
        return Vec::new();
    }

    // an edit joins the previous hunk when their context windows touch: the
    // gap between them is at most twice the context.
    let mut groups: Vec<Vec<&Edit>> = Vec::new();
    for e in &edits {
        match groups.last_mut() {
            Some(g) if e.old.start <= g.last().unwrap().old.end + 2 * context => g.push(e),
            _ => groups.push(vec![e]),
        }
    }

    groups
        .into_iter()
        .map(|g| build_hunk(&a, &b, &g, context))
        .collect()
}

fn build_hunk(a: &[&[u8]], b: &[&[u8]], group: &[&Edit], context: usize) -> Hunk {
    let first = group[0];
    let last = group[group.len() - 1];
    let old_from = first.old.start.saturating_sub(context);
    let new_from = first.new.start.saturating_sub(context);
    let old_to = (last.old.end + context).min(a.len());
    let new_to = (last.new.end + context).min(b.len());

    let mut lines = Vec::new();
    let mut oi = old_from;
    for e in group {
        // leading context up to this edit
        while oi < e.old.start {
            lines.push((b' ', a[oi].to_vec()));
            oi += 1;
        }
        for k in e.old.clone() {
            lines.push((b'-', a[k].to_vec()));
        }
        for k in e.new.clone() {
            lines.push((b'+', b[k].to_vec()));
        }
        oi = e.old.end;
    }
    // trailing context
    while oi < old_to {
        lines.push((b' ', a[oi].to_vec()));
        oi += 1;
    }

    Hunk {
        old_start: old_from + 1,
        old_len: old_to - old_from,
        new_start: new_from + 1,
        new_len: new_to - new_from,
        lines,
    }
}

/// Renders the unified-diff body (hunks only, no `---`/`+++` file header) for
/// two buffers, appending to `out`. Returns whether anything differed. A
/// missing final newline on either side is annotated git-style with a
/// `\ No newline at end of file` marker.
pub fn write_unified(out: &mut Vec<u8>, old: &[u8], new: &[u8], context: usize) -> bool {
    let hs = hunks(old, new, context);
    if hs.is_empty() {
        return false;
    }
    let old_no_nl = !old.is_empty() && !old.ends_with(b"\n");
    let new_no_nl = !new.is_empty() && !new.ends_with(b"\n");
    for h in &hs {
        out.extend_from_slice(
            format!(
                "@@ -{} +{} @@\n",
                range_str(h.old_start, h.old_len),
                range_str(h.new_start, h.new_len)
            )
            .as_bytes(),
        );
        let n = h.lines.len();
        for (idx, (tag, line)) in h.lines.iter().enumerate() {
            out.push(*tag);
            out.extend_from_slice(line);
            if !line.ends_with(b"\n") {
                out.push(b'\n');
                // the last physical line of each side may lack a newline
                let is_old_tail = *tag != b'+';
                if (is_old_tail && old_no_nl) || (*tag == b'+' && new_no_nl) || idx == n - 1 {
                    out.extend_from_slice(b"\\ No newline at end of file\n");
                }
            }
        }
    }
    true
}

/// git's `@@` coordinate: `start,len`, or just `start` when `len == 1`, and
/// `start` is decremented to `0`-anchored when the range is empty.
fn range_str(start: usize, len: usize) -> String {
    if len == 1 {
        format!("{start}")
    } else if len == 0 {
        format!("{},0", start.saturating_sub(1))
    } else {
        format!("{start},{len}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &[u8], edits: &[Edit], new: &[u8]) -> bool {
        // reconstruct `new` from `old` + edits and compare
        let a = split_lines(old);
        let b = split_lines(new);
        let mut out: Vec<&[u8]> = Vec::new();
        let mut oi = 0;
        for e in edits {
            while oi < e.old.start {
                out.push(a[oi]);
                oi += 1;
            }
            for k in e.new.clone() {
                out.push(b[k]);
            }
            oi = e.old.end;
        }
        while oi < a.len() {
            out.push(a[oi]);
            oi += 1;
        }
        let rebuilt: Vec<u8> = out.concat();
        rebuilt == new
    }

    #[test]
    fn split_handles_trailing_newline_or_not() {
        assert_eq!(split_lines(b"a\nb\n"), vec![&b"a\n"[..], &b"b\n"[..]]);
        assert_eq!(split_lines(b"a\nb"), vec![&b"a\n"[..], &b"b"[..]]);
        assert_eq!(split_lines(b""), Vec::<&[u8]>::new());
    }

    #[test]
    fn identical_is_empty() {
        assert!(diff(b"a\nb\nc\n", b"a\nb\nc\n").is_empty());
    }

    #[test]
    fn single_line_change() {
        let e = diff(b"a\nb\nc\n", b"a\nB\nc\n");
        assert_eq!(
            e,
            vec![Edit {
                old: 1..2,
                new: 1..2
            }]
        );
        assert!(apply(b"a\nb\nc\n", &e, b"a\nB\nc\n"));
    }

    #[test]
    fn pure_insertion_and_deletion() {
        let ins = diff(b"a\nc\n", b"a\nb\nc\n");
        assert_eq!(
            ins,
            vec![Edit {
                old: 1..1,
                new: 1..2
            }]
        );
        let del = diff(b"a\nb\nc\n", b"a\nc\n");
        assert_eq!(
            del,
            vec![Edit {
                old: 1..2,
                new: 1..1
            }]
        );
    }

    #[test]
    fn anchors_on_the_rare_line() {
        // many duplicate lines around one unique change; histogram should
        // not smear the edit across the duplicates
        let old = b"x\nx\nUNIQUE\nx\nx\n";
        let new = b"x\nx\nCHANGED\nx\nx\n";
        let e = diff(old, new);
        assert_eq!(
            e,
            vec![Edit {
                old: 2..3,
                new: 2..3
            }]
        );
        assert!(apply(old, &e, new));
    }

    #[test]
    fn unified_format_basic() {
        let mut out = Vec::new();
        let changed = write_unified(&mut out, b"a\nb\nc\n", b"a\nB\nc\n", 1);
        assert!(changed);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("@@ -1,3 +1,3 @@\n"), "{s}");
        assert!(s.contains(" a\n-b\n+B\n c\n"), "{s}");
    }

    #[test]
    fn no_newline_marker() {
        let mut out = Vec::new();
        write_unified(&mut out, b"a\nb", b"a\nc", 1);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("\\ No newline at end of file"), "{s}");
    }

    #[test]
    fn binary_detection() {
        assert!(is_binary(b"abc\0def"));
        assert!(!is_binary(b"plain text\n"));
    }

    #[test]
    fn randomized_roundtrip_reconstructs_new() {
        // a deterministic xorshift drives thousands of random old/new pairs
        // over a tiny line alphabet (lots of duplicates, the histogram's
        // stress case); applying the diff must always rebuild `new`.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let alphabet = [&b"a\n"[..], b"b\n", b"c\n", b"d\n", b"e\n"];
        for _ in 0..5000 {
            let mk = |rng: &mut dyn FnMut() -> u64| -> Vec<u8> {
                let n = (rng() % 12) as usize;
                let mut s = Vec::new();
                for _ in 0..n {
                    s.extend_from_slice(alphabet[(rng() % alphabet.len() as u64) as usize]);
                }
                s
            };
            let old = mk(&mut rng);
            let new = mk(&mut rng);
            let edits = diff(&old, &new);
            assert!(
                apply(&old, &edits, &new),
                "roundtrip failed\nold={:?}\nnew={:?}\nedits={:?}",
                String::from_utf8_lossy(&old),
                String::from_utf8_lossy(&new),
                edits
            );
            // edits must be ordered and non-overlapping on the old side
            for w in edits.windows(2) {
                assert!(w[0].old.end <= w[1].old.start, "overlap: {edits:?}");
            }
        }
    }
}
