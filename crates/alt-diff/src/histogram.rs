//! The histogram LCS core: anchor on the *rarest* matching line, then recurse
//! on the flanks. Operates on interned token slices (one `u32` per line).
//!
//! For any input the emitted edits are a valid decomposition — the matched
//! runs are equal by construction — so applying them to `old` reproduces
//! `new`. Histogram's rarity preference only affects which valid alignment is
//! chosen (i.e. hunk quality), never correctness.

use std::collections::HashMap;
use std::ops::Range;

use crate::Edit;

/// git's cap on how many equal lines a token may have before it is considered
/// too common to anchor on.
const MAX_CHAIN: usize = 64;

/// Recursively diffs `a[a_range]` against `b[b_range]`, pushing changed
/// regions to `out` in increasing order.
pub fn diff(
    a: &[u32],
    a_range: Range<usize>,
    b: &[u32],
    b_range: Range<usize>,
    out: &mut Vec<Edit>,
) {
    let (mut alo, mut ahi) = (a_range.start, a_range.end);
    let (mut blo, mut bhi) = (b_range.start, b_range.end);

    // strip the common prefix and suffix: they need no edit and shrink the
    // region the quadratic search runs over.
    while alo < ahi && blo < bhi && a[alo] == b[blo] {
        alo += 1;
        blo += 1;
    }
    while alo < ahi && blo < bhi && a[ahi - 1] == b[bhi - 1] {
        ahi -= 1;
        bhi -= 1;
    }

    if alo == ahi || blo == bhi {
        // one side is empty after trimming: pure insertion/deletion (or, if
        // both empty, nothing at all)
        if alo != ahi || blo != bhi {
            out.push(Edit {
                old: alo..ahi,
                new: blo..bhi,
            });
        }
        return;
    }

    match find_lcs(a, alo, ahi, b, blo, bhi) {
        Some((as_, ae, bs, be)) => {
            diff(a, alo..as_, b, blo..bs, out);
            diff(a, ae..ahi, b, be..bhi, out);
        }
        None => out.push(Edit {
            old: alo..ahi,
            new: blo..bhi,
        }),
    }
}

/// Finds the best common run in the two regions: the one anchored on the
/// rarest line (fewest occurrences in `a`'s region), ties broken by length
/// then by earliest position. Returns `(a_start, a_end, b_start, b_end)` as
/// half-open ranges, or `None` if the regions share no line.
fn find_lcs(
    a: &[u32],
    alo: usize,
    ahi: usize,
    b: &[u32],
    blo: usize,
    bhi: usize,
) -> Option<(usize, usize, usize, usize)> {
    // positions of each token within a's region, ascending
    let mut positions: HashMap<u32, Vec<usize>> = HashMap::new();
    for (ai, &tok) in a.iter().enumerate().take(ahi).skip(alo) {
        positions.entry(tok).or_default().push(ai);
    }

    // best so far: (a_start, a_end_inclusive, b_start, b_end_inclusive, rarity)
    let mut best: Option<(usize, usize, usize, usize, usize)> = None;

    let mut bi = blo;
    while bi < bhi {
        let Some(chain) = positions.get(&b[bi]) else {
            bi += 1;
            continue;
        };
        if chain.len() > MAX_CHAIN {
            bi += 1; // too common a line to be a good anchor
            continue;
        }
        for &ai in chain {
            // grow the aligned run around (ai, bi) in both directions
            let (mut sa, mut sb) = (ai, bi);
            while sa > alo && sb > blo && a[sa - 1] == b[sb - 1] {
                sa -= 1;
                sb -= 1;
            }
            let (mut ea, mut eb) = (ai, bi);
            while ea + 1 < ahi && eb + 1 < bhi && a[ea + 1] == b[eb + 1] {
                ea += 1;
                eb += 1;
            }
            // rarity = the fewest occurrences of any line in the run
            let rarity = (sa..=ea)
                .map(|k| positions.get(&a[k]).map_or(usize::MAX, Vec::len))
                .min()
                .unwrap_or(usize::MAX);
            let len = ea - sa + 1;
            let better = match best {
                None => true,
                Some((bsa, bea, _, _, br)) => {
                    let blen = bea - bsa + 1;
                    rarity < br || (rarity == br && len > blen)
                }
            };
            if better {
                best = Some((sa, ea, sb, eb, rarity));
            }
        }
        bi += 1;
    }

    best.map(|(sa, ea, sb, eb, _)| (sa, ea + 1, sb, eb + 1))
}
