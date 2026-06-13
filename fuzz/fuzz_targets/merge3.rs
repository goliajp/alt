//! Three-way merge on arbitrary input. Three byte buffers are carved from the
//! fuzz data; merging must never panic, and the identity invariants must hold
//! for any inputs: a region changed by only one side is taken from that side
//! exactly and never conflicts — `merge(base, X, base) == X` and
//! `merge(base, base, X) == X`.
#![no_main]

use alt_merge::{Labels, merge};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // carve three parts at positions the data itself chooses
    let n = data.len();
    let (p, q) = if n >= 2 {
        (
            data[0] as usize % (n + 1),
            data[n - 1] as usize % (n + 1),
        )
    } else {
        (0, n)
    };
    let (lo, hi) = (p.min(q), p.max(q));
    let base = &data[..lo];
    let x = &data[lo..hi];

    let labels = Labels::default();

    // general merge must not panic
    let _ = merge(base, &data[..hi], &data[lo..], &labels);

    // identity invariants
    let ours_only = merge(base, x, base, &labels);
    assert_eq!(ours_only.conflicts, 0, "ours-only must be clean");
    assert_eq!(ours_only.content, x, "merge(base, X, base) must equal X");

    let theirs_only = merge(base, base, x, &labels);
    assert_eq!(theirs_only.conflicts, 0, "theirs-only must be clean");
    assert_eq!(theirs_only.content, x, "merge(base, base, X) must equal X");
});
