//! Line-level three-way merge (diff3): given a common `base` and two
//! derivatives `ours` / `theirs`, produce the merged text, marking regions
//! that both sides changed incompatibly with git-style conflict markers.
//!
//! Built on `alt-diff`: the two edit scripts `base→ours` and `base→theirs`
//! are walked together; a region only conflicts when both sides changed the
//! same base lines to different content. A region changed by one side only is
//! taken cleanly.
//!
//! Pure logic: bytes in, bytes out, no business types or I/O.

use alt_diff::split_lines;

/// Labels written into conflict markers (`<<<<<<< ours` … `>>>>>>> theirs`).
pub struct Labels<'a> {
    pub ours: &'a str,
    pub theirs: &'a str,
}

impl Default for Labels<'_> {
    fn default() -> Self {
        Labels {
            ours: "ours",
            theirs: "theirs",
        }
    }
}

/// The result of a three-way merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    pub content: Vec<u8>,
    /// Number of conflict regions (0 = clean merge).
    pub conflicts: usize,
}

impl Merge {
    pub fn is_clean(&self) -> bool {
        self.conflicts == 0
    }
}

/// Three-way merges three byte buffers at line granularity.
pub fn merge(base: &[u8], ours: &[u8], theirs: &[u8], labels: &Labels) -> Merge {
    let o = split_lines(base);
    let a = split_lines(ours);
    let b = split_lines(theirs);

    let ea = alt_diff::diff_lines(&o, &a); // base → ours
    let eb = alt_diff::diff_lines(&o, &b); // base → theirs

    let mut out: Vec<u8> = Vec::new();
    let mut conflicts = 0;

    // current aligned positions in each side (oi in base, ai in ours, bi in
    // theirs); at every sync point ai/bi are the lines mapped to base[oi].
    let (mut oi, mut ai, mut bi) = (0usize, 0usize, 0usize);
    let (mut pa, mut pb) = (0usize, 0usize);

    loop {
        let a_here = pa < ea.len() && ea[pa].old.start == oi;
        let b_here = pb < eb.len() && eb[pb].old.start == oi;

        if !a_here && !b_here {
            if oi >= o.len() {
                break; // all base consumed and no edits remain at the tail
            }
            // a line identical across all three: emit and step in lockstep
            out.extend_from_slice(o[oi]);
            oi += 1;
            ai += 1;
            bi += 1;
            continue;
        }

        // a divergent region starts here; grow it to the next sync point,
        // chaining edits from either side that touch the running end
        let region_start = oi;
        let mut end = oi;
        let (mut na, mut nb) = (pa, pb);
        loop {
            let mut grew = false;
            while na < ea.len() && ea[na].old.start <= end {
                end = end.max(ea[na].old.end);
                na += 1;
                grew = true;
            }
            while nb < eb.len() && eb[nb].old.start <= end {
                end = end.max(eb[nb].old.end);
                nb += 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }

        // net line-count change each side applies inside the region
        let delta_a: isize = ea[pa..na]
            .iter()
            .map(|e| len(&e.new) as isize - len(&e.old) as isize)
            .sum();
        let delta_b: isize = eb[pb..nb]
            .iter()
            .map(|e| len(&e.new) as isize - len(&e.old) as isize)
            .sum();
        let span = end - region_start;
        let a_end = (ai as isize + span as isize + delta_a) as usize;
        let b_end = (bi as isize + span as isize + delta_b) as usize;

        let base_slice = &o[region_start..end];
        let ours_slice = &a[ai..a_end];
        let theirs_slice = &b[bi..b_end];

        if slices_eq(ours_slice, base_slice) {
            // only theirs changed this region
            push_lines(&mut out, theirs_slice);
        } else if slices_eq(theirs_slice, base_slice) {
            // only ours changed this region
            push_lines(&mut out, ours_slice);
        } else if slices_eq(ours_slice, theirs_slice) {
            // both made the same change
            push_lines(&mut out, ours_slice);
        } else {
            // genuine conflict
            conflicts += 1;
            ensure_newline(&mut out);
            out.extend_from_slice(format!("<<<<<<< {}\n", labels.ours).as_bytes());
            push_lines_nl(&mut out, ours_slice);
            out.extend_from_slice(b"=======\n");
            push_lines_nl(&mut out, theirs_slice);
            out.extend_from_slice(format!(">>>>>>> {}\n", labels.theirs).as_bytes());
        }

        oi = end;
        ai = a_end;
        bi = b_end;
        pa = na;
        pb = nb;
    }

    Merge {
        content: out,
        conflicts,
    }
}

fn len(r: &std::ops::Range<usize>) -> usize {
    r.end - r.start
}

fn slices_eq(x: &[&[u8]], y: &[&[u8]]) -> bool {
    x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p == q)
}

fn push_lines(out: &mut Vec<u8>, lines: &[&[u8]]) {
    for l in lines {
        out.extend_from_slice(l);
    }
}

/// Like [`push_lines`] but guarantees each emitted line ends in `\n`, so a
/// following conflict marker lands on its own line even when a side's last
/// line lacked a trailing newline.
fn push_lines_nl(out: &mut Vec<u8>, lines: &[&[u8]]) {
    for l in lines {
        out.extend_from_slice(l);
        if !l.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
}

fn ensure_newline(out: &mut Vec<u8>) {
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(base: &str, ours: &str, theirs: &str) -> Merge {
        merge(
            base.as_bytes(),
            ours.as_bytes(),
            theirs.as_bytes(),
            &Labels::default(),
        )
    }

    #[test]
    fn no_changes_returns_base() {
        let r = m("a\nb\nc\n", "a\nb\nc\n", "a\nb\nc\n");
        assert!(r.is_clean());
        assert_eq!(r.content, b"a\nb\nc\n");
    }

    #[test]
    fn one_side_change_is_taken_cleanly() {
        // only ours changes line 2
        let r = m("a\nb\nc\n", "a\nB\nc\n", "a\nb\nc\n");
        assert!(r.is_clean());
        assert_eq!(r.content, b"a\nB\nc\n");
        // only theirs changes line 2
        let r = m("a\nb\nc\n", "a\nb\nc\n", "a\nT\nc\n");
        assert!(r.is_clean());
        assert_eq!(r.content, b"a\nT\nc\n");
    }

    #[test]
    fn disjoint_changes_both_apply() {
        // ours edits the first line, theirs the last — no overlap
        let r = m("a\nb\nc\n", "A\nb\nc\n", "a\nb\nC\n");
        assert!(r.is_clean(), "{:?}", String::from_utf8_lossy(&r.content));
        assert_eq!(r.content, b"A\nb\nC\n");
    }

    #[test]
    fn identical_change_on_both_sides_is_clean() {
        let r = m("a\nb\nc\n", "a\nX\nc\n", "a\nX\nc\n");
        assert!(r.is_clean());
        assert_eq!(r.content, b"a\nX\nc\n");
    }

    #[test]
    fn conflicting_change_gets_markers() {
        let r = m("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        assert_eq!(r.conflicts, 1);
        let s = String::from_utf8(r.content).unwrap();
        assert_eq!(
            s,
            "a\n<<<<<<< ours\nOURS\n=======\nTHEIRS\n>>>>>>> theirs\nc\n"
        );
    }

    #[test]
    fn both_insert_different_blocks_at_same_point_conflicts() {
        let r = m("a\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        assert_eq!(r.conflicts, 1);
    }

    #[test]
    fn insertions_on_one_side_only_are_clean() {
        let r = m("a\nc\n", "a\nb\nc\n", "a\nc\n");
        assert!(r.is_clean());
        assert_eq!(r.content, b"a\nb\nc\n");
    }

    #[test]
    fn delete_on_one_side_modify_on_other_conflicts() {
        // ours deletes b, theirs changes it -> conflict
        let r = m("a\nb\nc\n", "a\nc\n", "a\nB\nc\n");
        assert_eq!(r.conflicts, 1);
    }

    #[test]
    fn randomized_identity_invariants() {
        // For any inputs: merging when only one side changed must yield that
        // side exactly and never conflict. merge(base,X,base)==X and
        // merge(base,base,X)==X. A deterministic xorshift drives the cases
        // over a tiny alphabet (the diff3 stress shape, lots of duplicates).
        let mut state: u64 = 0xD1B54A32D192ED03;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let alphabet = [&b"a\n"[..], b"b\n", b"c\n", b"d\n"];
        for _ in 0..4000 {
            let mk = |rng: &mut dyn FnMut() -> u64| -> Vec<u8> {
                let n = (rng() % 10) as usize;
                let mut s = Vec::new();
                for _ in 0..n {
                    s.extend_from_slice(alphabet[(rng() % alphabet.len() as u64) as usize]);
                }
                s
            };
            let base = mk(&mut rng);
            let x = mk(&mut rng);
            let ours_only = merge(&base, &x, &base, &Labels::default());
            assert!(ours_only.is_clean(), "ours-only must not conflict");
            assert_eq!(ours_only.content, x, "merge(base,X,base) must equal X");
            let theirs_only = merge(&base, &base, &x, &Labels::default());
            assert!(theirs_only.is_clean(), "theirs-only must not conflict");
            assert_eq!(theirs_only.content, x, "merge(base,base,X) must equal X");
        }
    }
}
