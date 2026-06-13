//! Histogram diff on arbitrary input. Two byte buffers are carved from the
//! fuzz data; diffing them must never panic, and applying the edits to `old`
//! must reproduce `new` exactly — the algorithm's core correctness invariant.
#![no_main]

use alt_diff::{Edit, diff, split_lines};
use libfuzzer_sys::fuzz_target;

fn apply(old: &[u8], edits: &[Edit], new: &[u8]) -> Vec<u8> {
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
    out.concat()
}

fuzz_target!(|data: &[u8]| {
    // split the input at a byte the data itself chooses, so both sides vary
    let split = data.first().copied().unwrap_or(0) as usize;
    let at = if data.len() > 1 {
        1 + split % (data.len() - 1)
    } else {
        data.len()
    };
    let (old, new) = data.split_at(at.min(data.len()));

    let edits = diff(old, new);
    // edits stay ordered and non-overlapping on the old side
    for w in edits.windows(2) {
        assert!(w[0].old.end <= w[1].old.start, "overlapping edits");
    }
    let rebuilt = apply(old, &edits, new);
    assert_eq!(rebuilt, new, "diff did not reconstruct new");
});
