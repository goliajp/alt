//! Zstandard (RFC 8878) compression and decompression.
//!
//! The decoder accepts any conforming frame without a dictionary; the encoder
//! writes conforming frames that any Zstandard decoder can read. Both come in
//! a plain form and in a form that takes a *prefix*: bytes treated as history
//! located just before the data, which is how one version of a file is stored
//! as a patch against another.

mod bits;
mod enc;
mod execute;
mod frame;
mod fse;
mod huffman;
mod literals;
mod sequences;
mod weights;
mod xxh64;

/// Whether the processor has the bit-manipulation instructions that the
/// hot loops are compiled a second time for.
#[cfg(target_arch = "x86_64")]
pub(crate) fn has_bmi2() -> bool {
    std::is_x86_feature_detected!("bmi2")
        && std::is_x86_feature_detected!("bmi1")
        && std::is_x86_feature_detected!("lzcnt")
}

/// Why a decode failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// the input ends in the middle of a frame
    Truncated,
    /// the input does not start with a Zstandard frame
    BadMagic,
    /// the frame needs a dictionary or sets a reserved bit
    Unsupported,
    /// the frame contradicts the format
    Corrupt,
    /// the content does not match the checksum stored after it
    Checksum,
    /// the content is not as long as the frame or the caller says
    SizeMismatch,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Truncated => "zstd data ends early",
            Self::BadMagic => "not a zstd frame",
            Self::Unsupported => "zstd frame needs a dictionary or uses a reserved feature",
            Self::Corrupt => "corrupt zstd data",
            Self::Checksum => "zstd content fails its checksum",
            Self::SizeMismatch => "zstd content has the wrong length",
        })
    }
}

impl std::error::Error for Error {}

/// Decompresses `src`, which holds one or more concatenated frames
/// (skippable frames are ignored) and nothing else.
///
/// The content size stated in a frame header is verified but never trusted
/// for allocation, and a content checksum is verified when present. Window
/// sizes are not limited: the output buffer is the window.
pub fn decode_all(src: &[u8]) -> Result<Vec<u8>, Error> {
    frame::decode(src, &[], usize::MAX)
}

/// Decompresses `src` with `prefix` as the history that precedes the first
/// frame, the counterpart of [`encode_with_prefix`]. Frames after the first
/// see no prefix.
///
/// Fails with [`Error::SizeMismatch`] unless the result is exactly
/// `expected_len` bytes long; decoding stops as soon as it would exceed that.
/// A buffer of `expected_len` bytes is allocated up front.
pub fn decode_with_prefix(
    src: &[u8],
    prefix: &[u8],
    expected_len: usize,
) -> Result<Vec<u8>, Error> {
    let out = frame::decode(src, prefix, expected_len)?;
    if out.len() != expected_len {
        return Err(Error::SizeMismatch);
    }
    Ok(out)
}

/// Compresses `data` into one frame.
///
/// Two strengths exist: levels 1 and 2 (and negative levels) use a
/// single-table greedy matcher, level 3 and above use a two-table matcher
/// that finds more. Level 0 means the default, 3.
///
/// Each thread that compresses keeps its hash tables (up to 768 KiB) and
/// block buffers for the next call.
pub fn encode_all(data: &[u8], level: i32) -> Vec<u8> {
    enc::encode(data, &[], level)
}

/// Compresses `data` into one frame that may refer to `prefix`, so that data
/// resembling the prefix costs little. Decode with [`decode_with_prefix`]
/// and the same prefix.
///
/// The frame declares a window that covers the prefix and the data, up to
/// 128 MiB, the most a decoder accepts by default; of a longer prefix only
/// the final 128 MiB are searched. `level` is as for [`encode_all`].
pub fn encode_with_prefix(data: &[u8], prefix: &[u8], level: i32) -> Vec<u8> {
    enc::encode(data, prefix, level)
}
