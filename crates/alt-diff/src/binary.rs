//! Chunk-level binary diff (A8 B1): given two byte streams that the text
//! engine considers binary, report how many CDC chunks they share, how many
//! are new on each side, and what fraction of the bytes are shared.
//!
//! For any binary file, this answers "what fraction is genuinely the same
//! content" — the basic question git can't even ask, because git's blob is
//! one opaque hash. Building on [`alt_cdc`] means the same FastCDC cut
//! points the store uses for dedup are what the diff sees, so a file
//! committed under one alt revision and looked at under another is reported
//! in the same units it was stored in.
//!
//! ## What this does *not* do
//!
//! Ordered edit ops (insert / delete / move at chunk granularity) are out
//! of scope here — they require an LCS over the chunk-OID sequence, which
//! costs O(N×M) in the chunk counts and only pays off when both sides have
//! < a few thousand chunks. A8 design §3.1 makes this a `--full`-mode opt-in
//! when the engine grows it; the default summary stays multiset-only.
//!
//! ## Determinism
//!
//! For fixed CDC [`alt_cdc::Params`], the chunking of `old` and `new` is a
//! pure function of bytes — so [`chunk_diff`] is deterministic.
//!
//! ## Fuzz invariants
//!
//! For arbitrary `old`/`new` (including empty, all-zeros, repeated byte,
//! megabyte-class random):
//! - no panic (the CDC chunker and the multiset arithmetic both panic-free
//!   on any input)
//! - `shared + added == new_chunks` and `shared + removed == old_chunks`
//! - `byte_shared_ratio ∈ [0.0, 1.0]` (saturating to 1.0 when both sides
//!   are empty)

use std::collections::HashMap;

pub use alt_cdc::{DEFAULT_PARAMS, Params};

/// A coarse-grained, machine-first chunk diff of two byte streams. All
/// counts are over the CDC chunking under the supplied [`Params`]; sums and
/// ratios are over the original bytes (so a tiny number of large new chunks
/// can still be reported as most of the change).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDiff {
    /// Number of distinct chunk occurrences shared between sides (multiset
    /// intersection size — a chunk that appears twice on each side counts
    /// twice).
    pub shared_chunks: usize,
    /// Chunks present on the new side but not balanced on the old side.
    pub added_chunks: usize,
    /// Chunks present on the old side but not balanced on the new side.
    pub removed_chunks: usize,
    /// Total chunks the old side was cut into.
    pub old_chunks: usize,
    /// Total chunks the new side was cut into.
    pub new_chunks: usize,
    /// Total bytes on the old side (sum of chunk sizes; equals `old.len()`).
    pub old_bytes: usize,
    /// Total bytes on the new side (equals `new.len()`).
    pub new_bytes: usize,
    /// Bytes attributable to chunks that are in the shared multiset, on the
    /// new side (so a chunk shared twice on the new side and once on the
    /// old contributes only once — what's *kept* from the old side).
    pub shared_bytes: usize,
}

impl ChunkDiff {
    /// Fraction of the old side's bytes that are also present, by chunk, on
    /// the new side. `1.0` if both inputs are empty, `0.0` if old is empty
    /// and new is not (nothing of the old is kept — there was no old).
    pub fn byte_shared_ratio(&self) -> f64 {
        let denom = self.old_bytes.max(self.new_bytes);
        if denom == 0 {
            // Both empty: trivially "fully shared". Anything else divides.
            return 1.0;
        }
        self.shared_bytes as f64 / denom as f64
    }
}

/// Compute the chunk-level diff between two byte streams using the given
/// FastCDC params. Pass `alt_cdc::DEFAULT_PARAMS` to match the alt object
/// store's cuts (so this diff reports in the same units the store used to
/// dedup the data on the way in).
pub fn chunk_diff(old: &[u8], new: &[u8], params: Params) -> ChunkDiff {
    // (chunk-bytes → (count_old, count_new, chunk_size)). Hashing by the
    // raw chunk bytes is fine here — collisions in a `HashMap` are
    // structurally handled, and the dedup is exact (we compare slices, not
    // just hash values). For very large inputs a content hash (BLAKE3
    // truncated, as the odb does) would let us hash once per chunk; the
    // store's chunk index already does that, and a future overload will
    // accept pre-hashed chunk sequences.
    let mut table: HashMap<&[u8], (usize, usize)> = HashMap::new();
    let mut old_chunks = 0usize;
    for c in alt_cdc::chunks(old, params) {
        table.entry(c).or_insert((0, 0)).0 += 1;
        old_chunks += 1;
    }
    let mut new_chunks = 0usize;
    for c in alt_cdc::chunks(new, params) {
        table.entry(c).or_insert((0, 0)).1 += 1;
        new_chunks += 1;
    }

    let mut shared_chunks = 0usize;
    let mut shared_bytes = 0usize;
    let mut added = 0usize;
    let mut removed = 0usize;
    for (chunk, (a, b)) in &table {
        let shared_here = (*a).min(*b);
        shared_chunks += shared_here;
        shared_bytes += shared_here * chunk.len();
        // multiset arithmetic: anything not matched on a side is unique to it
        removed += a.saturating_sub(shared_here);
        added += b.saturating_sub(shared_here);
    }

    ChunkDiff {
        shared_chunks,
        added_chunks: added,
        removed_chunks: removed,
        old_chunks,
        new_chunks,
        old_bytes: old.len(),
        new_bytes: new.len(),
        shared_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_cdc::DEFAULT_PARAMS;

    /// Smaller params keep tests fast and avoid sweeping the FastCDC `min`
    /// bound (~16KiB by default) under the rug. With min=64/avg=256/max=1024
    /// even 4-8KiB fixtures produce ~ a dozen chunks — enough surface to
    /// exercise the multiset arithmetic without writing megabytes.
    const SMALL: Params = Params {
        min: 64,
        avg: 256,
        max: 1024,
    };

    fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
        // tiny LCG — deterministic and dep-free; we just need cut points
        // to actually fire (constant bytes never trigger a CDC cut)
        let (a, c, m) = (6364136223846793005u64, 1442695040888963407u64, u64::MAX);
        let mut state = seed.wrapping_mul(a).wrapping_add(c);
        let mut out = vec![0u8; len];
        for byte in out.iter_mut() {
            state = state.wrapping_mul(a).wrapping_add(c) & m;
            *byte = (state >> 33) as u8;
        }
        out
    }

    #[test]
    fn identical_inputs_are_fully_shared() {
        let data = pseudo_random(8 * 1024, 1);
        let d = chunk_diff(&data, &data, SMALL);
        assert_eq!(d.old_chunks, d.new_chunks);
        assert_eq!(d.shared_chunks, d.old_chunks);
        assert_eq!(d.added_chunks, 0);
        assert_eq!(d.removed_chunks, 0);
        assert!(
            (d.byte_shared_ratio() - 1.0).abs() < 1e-9,
            "expected full share, got {}",
            d.byte_shared_ratio()
        );
    }

    #[test]
    fn disjoint_inputs_share_nothing() {
        let a = pseudo_random(8 * 1024, 7);
        let b = pseudo_random(8 * 1024, 99);
        let d = chunk_diff(&a, &b, SMALL);
        assert_eq!(d.shared_chunks, 0, "different streams must not overlap");
        assert_eq!(d.added_chunks, d.new_chunks);
        assert_eq!(d.removed_chunks, d.old_chunks);
        assert_eq!(d.shared_bytes, 0);
        assert_eq!(d.byte_shared_ratio(), 0.0);
    }

    #[test]
    fn empty_vs_empty_is_trivially_shared() {
        let d = chunk_diff(&[], &[], SMALL);
        assert_eq!(d.old_chunks, 0);
        assert_eq!(d.new_chunks, 0);
        assert_eq!(d.shared_chunks, 0);
        assert_eq!(d.byte_shared_ratio(), 1.0, "empty/empty saturates to 1.0");
    }

    #[test]
    fn empty_old_against_new_is_all_added() {
        let new = pseudo_random(4 * 1024, 3);
        let d = chunk_diff(&[], &new, SMALL);
        assert_eq!(d.old_chunks, 0);
        assert_eq!(d.shared_chunks, 0);
        assert_eq!(d.removed_chunks, 0);
        assert_eq!(d.added_chunks, d.new_chunks);
        assert_eq!(d.byte_shared_ratio(), 0.0);
    }

    /// An interior insertion: the bytes around the insertion still hash to
    /// the same CDC chunks at most cut points, so most chunks are shared.
    /// (We don't pin an exact share count — the FastCDC cut at the
    /// boundary is data-dependent — but we require *some* sharing on the
    /// matched bytes and *some* added chunks at the inserted span.)
    #[test]
    fn interior_insertion_keeps_most_chunks_shared() {
        let prefix = pseudo_random(8 * 1024, 11);
        let suffix = pseudo_random(8 * 1024, 22);
        let mut old = Vec::with_capacity(prefix.len() + suffix.len());
        old.extend_from_slice(&prefix);
        old.extend_from_slice(&suffix);
        let inserted = pseudo_random(2 * 1024, 33);
        let mut new = Vec::with_capacity(old.len() + inserted.len());
        new.extend_from_slice(&prefix);
        new.extend_from_slice(&inserted);
        new.extend_from_slice(&suffix);

        let d = chunk_diff(&old, &new, SMALL);
        assert!(
            d.shared_chunks > 0,
            "interior insertion must keep some shared chunks: {d:?}"
        );
        assert!(
            d.added_chunks > 0,
            "interior insertion must surface added chunks: {d:?}"
        );
        // Shift-resistance: with FastCDC most of the old bytes should still
        // align — at least half of the new bytes ought to be shared. (Exact
        // ratio is data-dependent; a loose lower bound stays robust.)
        assert!(
            d.byte_shared_ratio() > 0.5,
            "expected >50% byte share through an interior insertion, got {}",
            d.byte_shared_ratio()
        );
    }

    /// Multiset behaviour: doubling the input doubles each chunk's count,
    /// so all of `old`'s chunks are shared (every chunk appears at least
    /// once on each side) and the second copy's chunks surface as added.
    /// We don't pin the chunk count — the CDC cut inside the buffer is
    /// data-dependent — only the multiset arithmetic.
    #[test]
    fn repeated_chunks_balance_multiset_style() {
        let unit = pseudo_random(512, 5);
        let old = unit.clone();
        let mut new = Vec::with_capacity(2 * unit.len());
        new.extend_from_slice(&unit);
        new.extend_from_slice(&unit);

        let d = chunk_diff(&old, &new, SMALL);
        assert!(d.old_chunks > 0, "old must chunk: {d:?}");
        assert_eq!(
            d.shared_chunks, d.old_chunks,
            "every old chunk should be shared with the new doubled stream: {d:?}"
        );
        assert_eq!(
            d.added_chunks,
            d.new_chunks - d.shared_chunks,
            "the second copy's chunks must surface as added: {d:?}"
        );
        assert!(d.added_chunks > 0, "doubling adds chunks: {d:?}");
        assert_eq!(d.removed_chunks, 0);
    }

    /// Smoke test under the production DEFAULT_PARAMS — the same FastCDC
    /// parameters the odb uses. Just check that the function doesn't panic
    /// and returns coherent counts on a chunk-sized buffer.
    #[test]
    fn default_params_smoke() {
        let old = pseudo_random(128 * 1024, 41);
        let new = pseudo_random(128 * 1024, 42);
        let d = chunk_diff(&old, &new, DEFAULT_PARAMS);
        assert!(d.old_chunks > 0);
        assert!(d.new_chunks > 0);
        assert!(d.byte_shared_ratio() >= 0.0 && d.byte_shared_ratio() <= 1.0);
    }
}
