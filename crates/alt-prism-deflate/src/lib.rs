//! Deflate/zlib strip prism — the "universal key" (design/prisms.md §4):
//! most binary assets are structured containers wrapping transparently
//! deflated streams (PNG IDAT, zip members, PDF streams, git loose
//! objects), and that compression is CDC's enemy (a one-byte source change
//! reshuffles the whole deflate stream → zero shared chunks). Stripping it
//! exposes the underlying bytes to dedup.
//!
//! The catch is bit-exact reproduction: recompressing must reproduce the
//! original stream verbatim. Per the spike (design/prisms.md §5),
//! **the producer decides reproducibility** and the recompressor must be C
//! libz (zlib-rs makes different choices). So this prism inflates, then
//! finds the libz parameters that reproduce the input exactly, recording a
//! tiny parameter byte; if none in the tried grid match, it declines
//! (Tier 0) rather than ever diffing against a foreign encoder.
//!
//! Stone discipline: bounds its inflate output (decompression bomb) and
//! never panics on adversarial input.

use std::io::Read;

use alt_prism::{Decomposition, Prism, PrismId};

/// This prism's stable identity in the registry.
pub const DEFLATE_PRISM: PrismId = PrismId(1);

/// Inflate output ceiling: refuse a stream that expands beyond this, so a
/// decompression bomb can never exhaust memory. Real assets' streams are
/// far smaller; a bomb is rejected to Tier 0.
const MAX_INFLATED: usize = 512 << 20;

/// zlib levels tried in producer-likelihood order: git writes level 1,
/// general tools default to 6, optimizers use 9. A miss declines.
const LEVEL_GRID: [i32; 4] = [1, 6, 9, 8];

pub struct DeflatePrism;

impl Prism for DeflatePrism {
    fn id(&self) -> PrismId {
        DEFLATE_PRISM
    }

    fn decompose(&self, input: &[u8]) -> Option<Decomposition> {
        // a zlib stream starts with a two-byte header whose check makes the
        // first two bytes a multiple of 31; cheap reject for non-zlib input
        if input.len() < 6 || !u16::from_be_bytes([input[0], input[1]]).is_multiple_of(31) {
            return None;
        }
        let inflated = zlib_inflate(input)?;
        // find the libz level that reproduces the exact stream
        for level in LEVEL_GRID {
            if zlib_deflate(&inflated, level).as_deref() == Some(input) {
                return Some(Decomposition {
                    recipe: vec![level as u8],
                    parts: vec![inflated],
                });
            }
        }
        None // no grid parameter reproduces it: leave it to Tier 0
    }

    fn recompose(&self, recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
        let &[level] = recipe else { return None };
        let [data] = parts else { return None };
        zlib_deflate(data, level as i32)
    }
}

/// Inflates a zlib stream, bounded by [`MAX_INFLATED`]. `None` on malformed
/// input or a bomb.
fn zlib_inflate(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(input)
        .take(MAX_INFLATED as u64 + 1)
        .read_to_end(&mut out)
        .ok()?;
    if out.len() > MAX_INFLATED {
        return None; // expanded past the ceiling — treat as a bomb
    }
    Some(out)
}

/// Compresses with the C libz encoder (stock zlib's compression decisions
/// are the de-facto standard the producers used). `compress2` uses the same
/// `deflateInit` defaults (windowBits 15, memLevel 8, default strategy) git
/// and most tools do, so a level match reproduces their stream.
fn zlib_deflate(data: &[u8], level: i32) -> Option<Vec<u8>> {
    if !(0..=9).contains(&level) {
        return None;
    }
    // SAFETY: compress2 writes at most `dest_len` bytes (initialized to the
    // compressBound upper bound) and updates `dest_len` to the actual size.
    unsafe {
        let mut dest_len = libz_sys::compressBound(data.len() as libz_sys::uLong);
        let mut dest = vec![0u8; dest_len as usize];
        let rc = libz_sys::compress2(
            dest.as_mut_ptr(),
            &mut dest_len,
            data.as_ptr(),
            data.len() as libz_sys::uLong,
            level,
        );
        if rc != libz_sys::Z_OK {
            return None;
        }
        dest.truncate(dest_len as usize);
        Some(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_round_trips_arbitrary_data() {
        // proves the libz FFI deflate + inflate path works at all
        let data = b"the quick brown fox".repeat(100);
        let z = zlib_deflate(&data, 6).unwrap();
        assert_eq!(zlib_inflate(&z).unwrap(), data);
    }

    #[test]
    fn non_zlib_input_declines_cheaply() {
        assert!(DeflatePrism.decompose(b"not zlib at all").is_none());
        assert!(DeflatePrism.decompose(&[]).is_none());
    }

    #[test]
    #[ignore = "needs $ALT_CORPUS; validates exact reproduction on real git loose zlib streams"]
    fn reproduces_real_git_loose_streams() {
        let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS");
        // gitflow-loose keeps every object loose — i.e. 1413 real git zlib
        // streams (the spike's data; git writes level 1)
        let objects = std::path::Path::new(&corpus).join("gitflow-loose/.git/objects");
        let (mut total, mut decomposed) = (0u32, 0u32);
        for fanout in std::fs::read_dir(&objects).unwrap() {
            let dir = fanout.unwrap().path();
            if dir.file_name().unwrap().to_string_lossy().len() != 2 {
                continue; // pack/, info/
            }
            for obj in std::fs::read_dir(&dir).unwrap() {
                let bytes = std::fs::read(obj.unwrap().path()).unwrap();
                total += 1;
                if let Some(d) = DeflatePrism.decompose(&bytes) {
                    let parts: Vec<&[u8]> = d.parts.iter().map(Vec::as_slice).collect();
                    // the iron law in practice: what decomposed must reproduce
                    assert_eq!(
                        DeflatePrism.recompose(&d.recipe, &parts).unwrap(),
                        bytes,
                        "a decomposed stream must reproduce exactly"
                    );
                    decomposed += 1;
                }
            }
        }
        eprintln!("git loose streams reproduced exactly: {decomposed}/{total}");
        assert!(
            decomposed * 100 >= total * 99,
            "git loose should be ~100% reproducible, got {decomposed}/{total}"
        );
    }

    #[test]
    fn decompose_recompose_round_trips_a_level1_stream() {
        // git writes level 1; build such a stream and check the prism
        // recovers the level and reproduces the exact bytes
        let payload = b"commit 42\0tree deadbeef\nauthor someone".repeat(20);
        let stream = zlib_deflate(&payload, 1).unwrap();
        let d = DeflatePrism
            .decompose(&stream)
            .expect("a level-1 stream must decompose");
        assert_eq!(d.recipe, vec![1u8], "the recorded level is 1");
        assert_eq!(
            d.parts,
            vec![payload.to_vec()],
            "the part is the inflated data"
        );
        let parts: Vec<&[u8]> = d.parts.iter().map(Vec::as_slice).collect();
        assert_eq!(
            DeflatePrism.recompose(&d.recipe, &parts).unwrap(),
            stream,
            "recompose reproduces the exact stream"
        );
    }
}
