//! Content fingerprinting for "perceptual"-style diff hints on binary
//! files. The first format we know how to read is PNG (the universal-key
//! envelope for images — and the prism layer's primary trigger), which is
//! enough to surface "X% off" alongside the chunk-diff summary that
//! [`crate::binary`] already emits.
//!
//! The fingerprint is a 64-bit hash over the inflated payload, bucketed
//! into 64 equal slices: each bit = (slice sum > inflated mean). Hamming
//! distance / 64 gives a stable "fraction of buckets that disagree". It is
//! not a real psycho-visual metric (no DCT, no luma weighting, no resize
//! filter) — for that we'd pull in a real image decoder. But it tracks
//! actual content change far more closely than a byte diff over the raw
//! `.png` does (a one-pixel tweak reshuffles the whole zlib stream → zero
//! shared bytes pre-inflation), which is the entire reason this exists.
//!
//! Stone discipline: never panics on adversarial input, bounded
//! decompression (inherits the deflate prism's ceiling). Returns `None`
//! when the input isn't a format we recognise.

use std::io::Read;

/// Inflation ceiling per file — matches the deflate prism's bomb guard so
/// an attacker-crafted PNG header pointing at a 4 GiB zlib stream can't
/// exhaust memory.
const MAX_INFLATED: usize = 64 << 20;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// What kind of binary content we recognised. Drives both the "kind" we
/// surface in the human / JSON output and the path we take for the
/// fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    /// Real PNG (8-byte signature checked; chunk walk verified far enough
    /// to find IDAT). Fingerprint comes from the inflated IDAT payload.
    Png,
}

impl ContentKind {
    /// Stable string for logs / JSON. Match the lowercase form used in
    /// prism ids so a `prism=png` field would line up downstream.
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentKind::Png => "png",
        }
    }
}

/// Best-effort content detection from the first few bytes. Today only PNG
/// — JPEG / PDF can grow under the same enum without rework. `None` =
/// "not a kind we know how to fingerprint"; the caller falls back to the
/// chunk-diff summary alone.
pub fn detect(data: &[u8]) -> Option<ContentKind> {
    if data.starts_with(PNG_SIGNATURE) {
        Some(ContentKind::Png)
    } else {
        None
    }
}

/// Inflated-content fingerprint for `data`, when [`detect`] recognises it.
/// `None` means: not a known kind, or the format-specific decoder
/// declined (corrupt PNG, truncated IDAT, oversized inflate). The caller
/// drops to the chunk-diff summary alone — never panics, never claims a
/// metric it can't compute.
pub fn fingerprint(data: &[u8]) -> Option<Fingerprint> {
    match detect(data)? {
        ContentKind::Png => png_fingerprint(data),
    }
}

/// One file's fingerprint: the recognised kind + a 64-bit hash over its
/// canonical inflated content.
#[derive(Debug, Clone, Copy)]
pub struct Fingerprint {
    pub kind: ContentKind,
    pub hash: u64,
}

/// Fraction of bits that differ between two fingerprints, only meaningful
/// when both `Some` *and* their kinds match (otherwise comparing apples
/// and oranges). Returned in [0.0, 1.0]; 0.0 = identical, 1.0 = inverted.
pub fn distance(old: Option<Fingerprint>, new: Option<Fingerprint>) -> Option<f64> {
    let (o, n) = (old?, new?);
    if o.kind != n.kind {
        return None;
    }
    let diff_bits = (o.hash ^ n.hash).count_ones();
    Some(diff_bits as f64 / 64.0)
}

fn png_fingerprint(data: &[u8]) -> Option<Fingerprint> {
    let idat = png_idat(data)?;
    let inflated = inflate(&idat)?;
    Some(Fingerprint {
        kind: ContentKind::Png,
        hash: hash_64(&inflated),
    })
}

/// Concatenates every IDAT chunk's payload. Walks the PNG chunk list,
/// tolerating other (ancillary) chunks. Returns `None` on any structural
/// problem so a corrupt PNG falls back to the chunk-diff alone.
fn png_idat(data: &[u8]) -> Option<Vec<u8>> {
    if !data.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut at = PNG_SIGNATURE.len();
    let mut idat = Vec::new();
    while at + 8 <= data.len() {
        let len = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
        let kind = &data[at + 4..at + 8];
        let body_start = at + 8;
        let body_end = body_start.checked_add(len)?;
        // body + crc must fit
        if body_end + 4 > data.len() {
            return None;
        }
        if kind == b"IDAT" {
            idat.extend_from_slice(&data[body_start..body_end]);
        } else if kind == b"IEND" {
            break;
        }
        at = body_end + 4; // skip 4-byte CRC
    }
    if idat.is_empty() {
        return None;
    }
    Some(idat)
}

/// Bomb-bounded zlib inflate; gives up over [`MAX_INFLATED`].
fn inflate(stream: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = flate2::read::ZlibDecoder::new(stream).take(MAX_INFLATED as u64 + 1);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    if out.len() > MAX_INFLATED {
        return None;
    }
    Some(out)
}

/// Slices `data` into 64 equal-ish buckets and emits a 64-bit hash where
/// bit `i` is set iff bucket `i`'s mean is above the overall mean. Stable
/// across runs, monotonic-ish under "content shifted slightly". A bucket
/// past the end of `data` is treated as empty (its bit = 0).
fn hash_64(data: &[u8]) -> u64 {
    if data.is_empty() {
        return 0;
    }
    let total: u64 = data.iter().map(|&b| b as u64).sum();
    let overall_mean = total as f64 / data.len() as f64;
    let mut hash = 0u64;
    for i in 0..64 {
        let lo = i * data.len() / 64;
        let hi = (i + 1) * data.len() / 64;
        if lo >= hi {
            continue;
        }
        let bucket_sum: u64 = data[lo..hi].iter().map(|&b| b as u64).sum();
        let bucket_mean = bucket_sum as f64 / (hi - lo) as f64;
        if bucket_mean > overall_mean {
            hash |= 1u64 << i;
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid 8-bit grayscale PNG with `width × height`
    /// pixels supplied by `pixels[row][col]`. Single IDAT, filter byte 0
    /// per scanline. Good enough for a unit test; intentionally not a
    /// general PNG encoder.
    fn build_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;
        assert_eq!(pixels.len() as u32, width * height);

        let mut out = Vec::new();
        out.extend_from_slice(PNG_SIGNATURE);

        // IHDR
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.push(8); // bit depth
        ihdr.push(0); // color type: greyscale
        ihdr.push(0); // compression
        ihdr.push(0); // filter
        ihdr.push(0); // interlace
        write_chunk(&mut out, b"IHDR", &ihdr);

        // IDAT: per scanline filter byte (0 = None) + raw bytes
        let mut raw = Vec::with_capacity((width * height + height) as usize);
        for row in 0..height as usize {
            raw.push(0); // filter type
            let start = row * width as usize;
            raw.extend_from_slice(&pixels[start..start + width as usize]);
        }
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(&raw).unwrap();
        let stream = encoder.finish().unwrap();
        write_chunk(&mut out, b"IDAT", &stream);

        write_chunk(&mut out, b"IEND", &[]);
        out
    }

    /// A real PNG would put a CRC32 in the trailing 4 bytes. Our `png_idat`
    /// walker only checks for length + IDAT/IEND, so a zero CRC is fine
    /// here. (No PNG parser the test interacts with verifies the CRC.)
    fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0; 4]); // placeholder CRC
    }

    #[test]
    fn detect_matches_png_signature_only() {
        let png = build_png(2, 1, &[0, 255]);
        assert_eq!(detect(&png), Some(ContentKind::Png));
        assert_eq!(detect(b"plain text"), None);
        assert_eq!(detect(b"\x89PNG"), None); // truncated, no full signature
    }

    #[test]
    fn distance_between_identical_pngs_is_zero() {
        let a = build_png(16, 16, &[42; 256]);
        let fa = fingerprint(&a).unwrap();
        assert_eq!(distance(Some(fa), Some(fa)), Some(0.0));
    }

    #[test]
    fn distance_grows_when_pixels_change() {
        // a checkerboard vs the same image with one quadrant inverted —
        // a meaningful chunk of bits should flip.
        let mut a = vec![0u8; 16 * 16];
        let mut b = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                let v = if (x + y) & 1 == 0 { 0 } else { 255 };
                a[y * 16 + x] = v;
                b[y * 16 + x] = v;
            }
        }
        // flip the top-left 8x8 in b
        for y in 0..8 {
            for x in 0..8 {
                b[y * 16 + x] = 255 - b[y * 16 + x];
            }
        }
        let pa = build_png(16, 16, &a);
        let pb = build_png(16, 16, &b);
        let fa = fingerprint(&pa).unwrap();
        let fb = fingerprint(&pb).unwrap();
        let d = distance(Some(fa), Some(fb)).unwrap();
        assert!(d > 0.0, "non-identical PNGs must have non-zero distance");
        assert!(d < 1.0, "distance saturates correctly");
    }

    #[test]
    fn distance_is_none_when_either_side_is_unknown() {
        let png = build_png(2, 1, &[0, 255]);
        let f = fingerprint(&png).unwrap();
        assert_eq!(distance(Some(f), None), None);
        assert_eq!(distance(None, Some(f)), None);
        assert_eq!(distance(None, None), None);
    }

    #[test]
    fn corrupt_png_falls_back_to_none() {
        // signature + truncated chunk
        let mut bad = PNG_SIGNATURE.to_vec();
        bad.extend_from_slice(&[0, 0, 0, 100]); // claims 100-byte chunk
        bad.extend_from_slice(b"IDAT"); // but nothing follows
        assert!(fingerprint(&bad).is_none());
    }
}
