//! PNG container prism — design/prisms.md §2, hot/cold list "png" row.
//!
//! A PNG is a signature + a chunk graph (IHDR, optional palette / colour
//! / text / time chunks, one or more IDAT chunks carrying a single
//! deflate-compressed image stream, and IEND). The bytes that *change*
//! between two versions of an image are inside that deflate stream;
//! every other chunk tends to be byte-identical.
//!
//! This prism splits a PNG into:
//!
//! - **envelope** — the file with the IDAT chunks' *data fields* cut
//!   out. Signature, IHDR, sRGB, gAMA, the IDAT length+type+CRC
//!   trailers, ancillary chunks, and IEND all stay verbatim. Because
//!   chunk CRCs are over `(type, data)` and the CRC bytes live in the
//!   envelope already, recomposed IDAT data must hash to the same CRC
//!   or the prism would have declined at ingest — the Tier 1 round
//!   trip would catch it.
//! - **one part** — the inflated IDAT bytes (the concatenation of every
//!   IDAT chunk's data, fed through zlib). This is what makes
//!   image-level dedup actually fire: a small edit on a PNG produces a
//!   wildly different deflate stream but the inflated payload is
//!   mostly the same, and downstream CDC happily chunks the inflated
//!   bytes.
//!
//! Iron law per alt-prism: we accept the decomposition only when libz
//! can reproduce the *exact* original concatenated IDAT body. design
//! §5 measured this at 1/10 on real-world PNGs — optipng/zopfli output
//! is not reproducible by any stock libz level. Those land at Tier 0
//! (the file stores verbatim, like git would), and that's fine: the
//! prism's value is the libz-produced PNG that ~every authoring tool
//! emits by default.

use std::io::Read;

use alt_prism::{Decomposition, Prism, PrismId};

pub const PNG_PRISM: PrismId = PrismId(3);

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Recipe magic for forward compatibility — a future revision (e.g.
/// "split IDAT into per-scanline filter rows") bumps this byte to keep
/// readers and writers unambiguous.
const RECIPE_MAGIC: &[u8] = b"ALTPNG01";

/// libz compression levels probed for IDAT in producer-likelihood order:
/// libpng / Pillow default 6, ImageMagick 9, then 8, then git-like 1.
const LEVEL_GRID: [i32; 4] = [6, 9, 8, 1];

/// Cap on the inflated IDAT payload, so an adversarial PNG can't OOM
/// the importer. Real PNGs of even very large dimensions stay well
/// inside this; anything above falls back to Tier 0.
const MAX_INFLATED: usize = 512 << 20;

/// Cap on the PNG itself.
const MAX_PNG: usize = 1 << 31;

pub struct PngPrism;

impl Prism for PngPrism {
    fn id(&self) -> PrismId {
        PNG_PRISM
    }

    fn decompose(&self, input: &[u8]) -> Option<Decomposition> {
        if input.len() < 8 + 12 || input.len() > MAX_PNG {
            return None;
        }
        if input[..8] != PNG_SIGNATURE {
            return None;
        }
        // Walk chunks. Each chunk = length u32 BE + type 4 bytes +
        // data + CRC u32. We accept anything we can parse; we only
        // care about identifying the IDAT data spans for the envelope
        // cut and the body splice during recompose.
        let mut idat_spans: Vec<DataSpan> = Vec::new();
        let mut at = 8;
        let mut saw_iend = false;
        while at + 12 <= input.len() {
            let length = u32::from_be_bytes(input[at..at + 4].try_into().ok()?) as usize;
            let ty = &input[at + 4..at + 8];
            let data_start = at + 8;
            let data_end = data_start.checked_add(length)?;
            let crc_end = data_end.checked_add(4)?;
            if crc_end > input.len() {
                return None;
            }
            if ty == b"IDAT" {
                idat_spans.push(DataSpan {
                    data_start,
                    data_end,
                });
            }
            if ty == b"IEND" {
                saw_iend = true;
                // PNG allows trailing bytes after IEND; we keep them
                // in the envelope so the round trip stays bit-exact.
            }
            at = crc_end;
        }
        if !saw_iend || idat_spans.is_empty() {
            return None;
        }
        if at != input.len() {
            // Some bytes after the last chunk we didn't recognise; we
            // still preserve them via the envelope, but reject if the
            // tail can't be cleanly accounted for.
            // (`at != input.len()` here means we stopped mid-chunk
            // header — a corrupted file.)
            return None;
        }

        // Concatenate IDAT data spans into a single zlib stream and
        // try to inflate.
        let mut zlib_stream = Vec::new();
        for span in &idat_spans {
            zlib_stream.extend_from_slice(&input[span.data_start..span.data_end]);
        }
        let inflated = zlib_inflate(&zlib_stream, MAX_INFLATED)?;
        // Find a libz level that reproduces the original zlib stream
        // verbatim. If none does, decline.
        let mut matched_level: Option<u8> = None;
        for level in LEVEL_GRID {
            if let Some(reco) = zlib_deflate(&inflated, level)
                && reco.len() == zlib_stream.len()
                && reco.as_slice() == zlib_stream.as_slice()
            {
                matched_level = Some(level as u8);
                break;
            }
        }
        let level = matched_level?;

        // Build the envelope: all bytes outside the IDAT data spans.
        // Spans are already in stream order because we walked chunks
        // left-to-right.
        let mut envelope = Vec::with_capacity(input.len());
        let mut cursor = 0usize;
        let mut span_meta: Vec<SpanMeta> = Vec::with_capacity(idat_spans.len());
        for span in &idat_spans {
            let seg_before = &input[cursor..span.data_start];
            envelope.extend_from_slice(seg_before);
            span_meta.push(SpanMeta {
                seg_before_len: seg_before.len() as u32,
                data_len: (span.data_end - span.data_start) as u32,
            });
            cursor = span.data_end;
        }
        let trailing = &input[cursor..];
        envelope.extend_from_slice(trailing);
        let seg_after_len = trailing.len() as u32;

        let recipe = encode_recipe(level, &span_meta, seg_after_len, &envelope);
        Some(Decomposition {
            recipe,
            parts: vec![inflated],
        })
    }

    fn recompose(&self, recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
        let (level, spans, seg_after_len, envelope) = decode_recipe(recipe)?;
        let [inflated] = parts else { return None };

        // Re-deflate at the recorded level — must match the original
        // zlib stream byte-for-byte, by construction of the iron law.
        let zlib_stream = zlib_deflate(inflated, level as i32)?;
        let total_data: u64 = spans.iter().map(|s| s.data_len as u64).sum();
        if zlib_stream.len() as u64 != total_data {
            return None;
        }
        let envelope_total: u64 =
            spans.iter().map(|s| s.seg_before_len as u64).sum::<u64>() + seg_after_len as u64;
        if envelope.len() as u64 != envelope_total {
            return None;
        }

        let mut out = Vec::with_capacity(envelope.len() + zlib_stream.len());
        let mut env_cursor = 0usize;
        let mut stream_cursor = 0usize;
        for span in &spans {
            let seg = span.seg_before_len as usize;
            let next = env_cursor.checked_add(seg)?;
            if next > envelope.len() {
                return None;
            }
            out.extend_from_slice(&envelope[env_cursor..next]);
            env_cursor = next;

            let data_len = span.data_len as usize;
            let stream_next = stream_cursor.checked_add(data_len)?;
            if stream_next > zlib_stream.len() {
                return None;
            }
            out.extend_from_slice(&zlib_stream[stream_cursor..stream_next]);
            stream_cursor = stream_next;
        }
        let final_start = env_cursor;
        let final_end = final_start.checked_add(seg_after_len as usize)?;
        if final_end != envelope.len() {
            return None;
        }
        out.extend_from_slice(&envelope[final_start..final_end]);
        Some(out)
    }
}

#[derive(Debug, Clone, Copy)]
struct DataSpan {
    data_start: usize,
    data_end: usize,
}

#[derive(Debug, Clone, Copy)]
struct SpanMeta {
    seg_before_len: u32,
    data_len: u32,
}

fn encode_recipe(level: u8, spans: &[SpanMeta], seg_after_len: u32, envelope: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 1 + 4 + 8 * spans.len() + 4 + 4 + envelope.len());
    out.extend_from_slice(RECIPE_MAGIC);
    out.push(level);
    out.extend_from_slice(&(spans.len() as u32).to_le_bytes());
    for s in spans {
        out.extend_from_slice(&s.seg_before_len.to_le_bytes());
        out.extend_from_slice(&s.data_len.to_le_bytes());
    }
    out.extend_from_slice(&seg_after_len.to_le_bytes());
    out.extend_from_slice(&(envelope.len() as u32).to_le_bytes());
    out.extend_from_slice(envelope);
    out
}

fn decode_recipe(recipe: &[u8]) -> Option<(u8, Vec<SpanMeta>, u32, Vec<u8>)> {
    if recipe.len() < 8 + 1 + 4 || &recipe[0..8] != RECIPE_MAGIC {
        return None;
    }
    let level = recipe[8];
    let n = u32::from_le_bytes(recipe[9..13].try_into().ok()?) as usize;
    let mut at = 13;
    let mut spans = Vec::with_capacity(n);
    for _ in 0..n {
        if at + 8 > recipe.len() {
            return None;
        }
        let seg_before_len = u32::from_le_bytes(recipe[at..at + 4].try_into().ok()?);
        let data_len = u32::from_le_bytes(recipe[at + 4..at + 8].try_into().ok()?);
        spans.push(SpanMeta {
            seg_before_len,
            data_len,
        });
        at += 8;
    }
    if at + 8 > recipe.len() {
        return None;
    }
    let seg_after_len = u32::from_le_bytes(recipe[at..at + 4].try_into().ok()?);
    let env_len = u32::from_le_bytes(recipe[at + 4..at + 8].try_into().ok()?) as usize;
    at += 8;
    if at + env_len != recipe.len() {
        return None;
    }
    Some((
        level,
        spans,
        seg_after_len,
        recipe[at..at + env_len].to_vec(),
    ))
}

fn zlib_inflate(input: &[u8], max: usize) -> Option<Vec<u8>> {
    if input.is_empty() {
        return None; // an IDAT zlib stream is never empty
    }
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(input)
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .ok()?;
    if out.len() > max {
        return None;
    }
    Some(out)
}

/// libz `compress2` produces a zlib-wrapped stream with the default
/// window / memLevel / strategy — exactly what every libpng-using
/// encoder does, so a level match reproduces the producer's stream
/// when the producer is also stock libz. No alloc shim needed (the
/// high-level helper allocates internally).
fn zlib_deflate(data: &[u8], level: i32) -> Option<Vec<u8>> {
    if !(0..=9).contains(&level) {
        return None;
    }
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

    /// Construct a minimal-but-real PNG: 8 bytes signature, IHDR,
    /// IDAT (one or more chunks compressing `payload`), IEND. Lengths
    /// and CRCs are written so libpng-style readers accept it.
    fn make_png(payload: &[u8], level: i32, idat_chunks: usize) -> Vec<u8> {
        let zlib = zlib_deflate(payload, level).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIGNATURE);

        // IHDR: 13 bytes content (we don't need the values to be
        // pixel-sane; the prism doesn't decode them).
        let ihdr = [0u8; 13];
        push_chunk(&mut out, b"IHDR", &ihdr);

        // Split the zlib stream across `idat_chunks` IDAT chunks so
        // we exercise the multi-chunk concatenation path.
        let part = zlib.len().div_ceil(idat_chunks.max(1));
        for slice in zlib.chunks(part) {
            push_chunk(&mut out, b"IDAT", slice);
        }
        push_chunk(&mut out, b"IEND", &[]);
        out
    }

    fn push_chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(ty);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(ty);
        crc_input.extend_from_slice(data);
        let crc = crc32_naive(&crc_input);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    fn crc32_naive(data: &[u8]) -> u32 {
        let mut crc: u32 = !0;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB88320 & mask);
            }
        }
        !crc
    }

    fn parts_refs(parts: &[Vec<u8>]) -> Vec<&[u8]> {
        parts.iter().map(|p| p.as_slice()).collect()
    }

    #[test]
    fn round_trips_single_idat_at_default_level() {
        let payload: Vec<u8> = (0..1024u32).flat_map(|i| (i as u8).to_le_bytes()).collect();
        let png = make_png(&payload, 6, 1);
        let d = PngPrism.decompose(&png).expect("decompose");
        assert_eq!(d.parts.len(), 1);
        assert_eq!(d.parts[0], payload);
        let reco = PngPrism
            .recompose(&d.recipe, &parts_refs(&d.parts))
            .expect("recompose");
        assert_eq!(reco, png);
    }

    #[test]
    fn round_trips_multi_idat_split_three_ways() {
        let payload: Vec<u8> = (0..4096u32).map(|i| (i as u8) ^ 0x55).collect();
        let png = make_png(&payload, 6, 3);
        let d = PngPrism.decompose(&png).expect("decompose");
        let reco = PngPrism
            .recompose(&d.recipe, &parts_refs(&d.parts))
            .expect("recompose");
        assert_eq!(reco, png);
    }

    #[test]
    fn round_trips_at_levels_9_and_1() {
        for level in [9, 1] {
            let payload: Vec<u8> = (0..2048u32).map(|i| (i as u8).wrapping_mul(7)).collect();
            let png = make_png(&payload, level, 1);
            let d = PngPrism.decompose(&png).expect("decompose");
            let reco = PngPrism
                .recompose(&d.recipe, &parts_refs(&d.parts))
                .expect("recompose");
            assert_eq!(reco, png);
        }
    }

    #[test]
    fn declines_non_png_input_quickly() {
        assert!(PngPrism.decompose(b"not a png").is_none());
        assert!(PngPrism.decompose(&[]).is_none());
        // ZIP signature
        assert!(PngPrism.decompose(b"PK\x03\x04zzzzzzzz").is_none());
    }

    #[test]
    fn declines_truncated_png_without_panicking() {
        let png = make_png(b"hello", 6, 1);
        let truncated = &png[..png.len() - 1];
        assert!(PngPrism.decompose(truncated).is_none());
    }

    #[test]
    fn recompose_rejects_wrong_part_count() {
        let png = make_png(b"hello", 6, 1);
        let d = PngPrism.decompose(&png).expect("decompose");
        assert!(PngPrism.recompose(&d.recipe, &[]).is_none());
        let extra = vec![d.parts[0].as_slice(), d.parts[0].as_slice()];
        assert!(PngPrism.recompose(&d.recipe, &extra).is_none());
    }
}
