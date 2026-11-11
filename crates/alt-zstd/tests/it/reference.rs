//! The reference implementation, driven the way the store used to drive it.

use zstd::zstd_safe::{CCtx, CParameter, DCtx, InBuffer, OutBuffer};

pub fn encode(data: &[u8], level: i32) -> Vec<u8> {
    zstd::encode_all(data, level).unwrap()
}

pub fn encode_checked(data: &[u8], level: i32) -> Vec<u8> {
    let mut encoder = zstd::Encoder::new(Vec::new(), level).unwrap();
    encoder.include_checksum(true).unwrap();
    std::io::Write::write_all(&mut encoder, data).unwrap();
    encoder.finish().unwrap()
}

pub fn decode(frame: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    zstd::decode_all(frame)
}

pub fn encode_with_prefix(data: &[u8], prefix: &[u8], level: i32) -> Vec<u8> {
    let mut context = CCtx::create();
    context
        .set_parameter(CParameter::CompressionLevel(level))
        .unwrap();
    context.ref_prefix(prefix).unwrap();
    let mut out = Vec::with_capacity(zstd::zstd_safe::compress_bound(data.len()));
    context.compress2(&mut out, data).unwrap();
    out
}

/// patch-from as the command-line tool does it: a window that covers the
/// prefix and long-distance matching
pub fn encode_patch(data: &[u8], prefix: &[u8], level: i32, window_log: u32) -> Vec<u8> {
    let mut context = CCtx::create();
    context
        .set_parameter(CParameter::CompressionLevel(level))
        .unwrap();
    context
        .set_parameter(CParameter::WindowLog(window_log))
        .unwrap();
    context
        .set_parameter(CParameter::EnableLongDistanceMatching(true))
        .unwrap();
    context.ref_prefix(prefix).unwrap();
    let mut out = Vec::with_capacity(zstd::zstd_safe::compress_bound(data.len()));
    context.compress2(&mut out, data).unwrap();
    out
}

/// streaming decode with default parameters after `ref_prefix`
pub fn decode_with_prefix(frame: &[u8], prefix: &[u8], len: usize) -> Option<Vec<u8>> {
    let mut context = DCtx::create();
    context.ref_prefix(prefix).ok()?;
    let mut out = Vec::with_capacity(len);
    let mut input = InBuffer::around(frame);
    let mut output = OutBuffer::around(&mut out);
    loop {
        let remaining = context.decompress_stream(&mut output, &mut input).ok()?;
        if remaining == 0 {
            break;
        }
        if output.pos() == output.capacity() || input.pos == input.src.len() {
            return None;
        }
    }
    Some(out)
}
