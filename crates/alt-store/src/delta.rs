//! Lineage delta codec: zstd with the base as a reference prefix
//! (patch-from semantics). The delta operates on the *decompressed* data
//! domain — the structural fix for git's binary-delta blindness — and the
//! base is named by content address, so re-encoding never disturbs
//! identity (encoding/identity decoupling).

/// Compresses `data` against `base` as a zstd ref-prefix.
pub fn compress_with_base(data: &[u8], base: &[u8], level: i32) -> Vec<u8> {
    alt_zstd::encode_with_prefix(data, base, level)
}

/// Decompresses a ref-prefix frame against `base` into exactly `orig_len`
/// bytes. Returns None on any mismatch (the caller reports corruption; it
/// never fabricates data).
pub fn decompress_with_base(payload: &[u8], base: &[u8], orig_len: usize) -> Option<Vec<u8>> {
    alt_zstd::decode_with_prefix(payload, base, orig_len).ok()
}
