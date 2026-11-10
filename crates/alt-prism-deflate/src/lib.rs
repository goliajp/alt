//! Deflate/zlib strip prism — the "universal key":
//! most binary assets are structured containers wrapping transparently
//! deflated streams (PNG IDAT, zip members, PDF streams, git loose
//! objects), and that compression is CDC's enemy (a one-byte source change
//! reshuffles the whole deflate stream → zero shared chunks). Stripping it
//! exposes the underlying bytes to dedup.
//!
//! The catch is bit-exact reproduction: recompressing must reproduce the
//! original stream verbatim. Measured on real inputs,
//! **the producer decides reproducibility** and the recompressor must make
//! libz's choices byte for byte (zlib-rs makes different ones). So this prism inflates, then
//! finds the libz parameters that reproduce the input exactly, recording a
//! tiny parameter byte; if none in the tried grid match, it declines
//! (Tier 0) rather than ever diffing against a foreign encoder.
//!
//! Discipline: bounds its inflate output (decompression bomb) and
//! never panics on adversarial input.

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
    // content past the ceiling is treated as a bomb
    let (out, _) = alt_inflate::inflate(alt_inflate::Format::Zlib, input, MAX_INFLATED).ok()?;
    Some(out)
}

/// Compresses the way stock zlib's `compress2` does (windowBits 15, memLevel 8,
/// default strategy), which is what git and most tools use, so a level match
/// reproduces their stream.
fn zlib_deflate(data: &[u8], level: i32) -> Option<Vec<u8>> {
    if !(0..=9).contains(&level) {
        return None;
    }
    Some(alt_deflate::compress_as_zlib(
        data,
        level as u8,
        alt_deflate::Format::Zlib,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deflate_round_trips_arbitrary_data() {
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
        // streams (git writes level 1)
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
