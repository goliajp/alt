//! Lineage delta codec: zstd with the base as a reference prefix
//! (patch-from semantics). The delta operates on the *decompressed* data
//! domain — the structural fix for git's binary-delta blindness — and the
//! base is named by content address, so re-encoding never disturbs
//! identity (encoding/identity decoupling).

use zstd::zstd_safe::{CCtx, DCtx, InBuffer, OutBuffer};

/// Compresses `data` against `base` as a zstd ref-prefix. Returns None on
/// any zstd-level failure (the caller falls back to a plain encoding).
pub fn compress_with_base(data: &[u8], base: &[u8], level: i32) -> Option<Vec<u8>> {
    let mut cctx = CCtx::create();
    cctx.set_parameter(zstd::zstd_safe::CParameter::CompressionLevel(level))
        .ok()?;
    cctx.ref_prefix(base).ok()?;
    let mut out = Vec::with_capacity(zstd::zstd_safe::compress_bound(data.len()));
    cctx.compress2(&mut out, data).ok()?;
    Some(out)
}

/// Decompresses a ref-prefix frame against `base`; `orig_len` bounds the
/// output buffer. Returns None on any mismatch (the caller reports
/// corruption; it never fabricates data).
pub fn decompress_with_base(payload: &[u8], base: &[u8], orig_len: usize) -> Option<Vec<u8>> {
    let mut dctx = DCtx::create();
    dctx.ref_prefix(base).ok()?;
    let mut out = Vec::with_capacity(orig_len);
    let mut in_buf = InBuffer::around(payload);
    let mut out_buf = OutBuffer::around(&mut out);
    loop {
        let remaining = dctx.decompress_stream(&mut out_buf, &mut in_buf).ok()?;
        if remaining == 0 {
            break;
        }
        if out_buf.pos() == out_buf.capacity() || in_buf.pos == in_buf.src.len() {
            // would need more room or more input than the record holds
            return None;
        }
    }
    let _ = out_buf;
    Some(out)
}
