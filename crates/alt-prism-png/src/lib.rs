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

/// Recipe magic v1: mode A — the part is the inflated IDAT payload
/// (still with per-scanline filter bytes attached). Older stores
/// containing v1 recipes are readable by this crate; the v2 path
/// (mode B) supersedes it for new writes when applicable.
const RECIPE_MAGIC_V1: &[u8] = b"ALTPNG01";

/// Recipe magic v2: mode B — the part is the *unfiltered* raw pixel
/// matrix (row_stride * height bytes, no filter bytes inline), and the
/// recipe records the per-row PNG filter type so recompose can re-apply
/// the original filtering before deflate. This is the granularity that
/// makes a small pixel edit dedup against the previous version's
/// scanlines: raw pixels are content-stable across reencoding, while
/// the filtered+deflated stream is not.
const RECIPE_MAGIC_V2: &[u8] = b"ALTPNG02";

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
        let parsed = parse_png(input)?;

        // Try mode B first (raw pixel matrix + per-row filter codes);
        // fall back to mode A (single inflated payload) when the PNG
        // sub-format isn't supported by mode B's unfilter path.
        if let Some(d) = decompose_mode_b(&parsed) {
            return Some(d);
        }
        decompose_mode_a(parsed)
    }

    fn recompose(&self, recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
        if recipe.len() < 8 {
            return None;
        }
        match &recipe[0..8] {
            m if m == RECIPE_MAGIC_V1 => recompose_mode_a(recipe, parts),
            m if m == RECIPE_MAGIC_V2 => recompose_mode_b(recipe, parts),
            _ => None,
        }
    }
}

/// Output of the shared PNG parse — every decompose path starts here.
struct ParsedPng<'a> {
    input: &'a [u8],
    idat_spans: Vec<DataSpan>,
    /// The inflated payload (still carries per-scanline filter bytes
    /// when interpreted as PNG filtered scanlines).
    inflated: Vec<u8>,
    /// libz level reproducing the concatenated zlib stream from
    /// `inflated` — already verified byte-exact (iron law). The
    /// concatenated stream itself is not retained; recompose
    /// regenerates it from the inflated bytes and the level.
    level: u8,
    /// Parsed IHDR; only available because every well-formed PNG has
    /// exactly one IHDR as the first chunk.
    ihdr: Ihdr,
}

#[derive(Debug, Clone, Copy)]
struct Ihdr {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlace: u8,
}

fn parse_png(input: &[u8]) -> Option<ParsedPng<'_>> {
    if input.len() < 8 + 12 || input.len() > MAX_PNG {
        return None;
    }
    if input[..8] != PNG_SIGNATURE {
        return None;
    }
    let mut idat_spans: Vec<DataSpan> = Vec::new();
    let mut at = 8;
    let mut saw_iend = false;
    let mut ihdr: Option<Ihdr> = None;
    while at + 12 <= input.len() {
        let length = u32::from_be_bytes(input[at..at + 4].try_into().ok()?) as usize;
        let ty = &input[at + 4..at + 8];
        let data_start = at + 8;
        let data_end = data_start.checked_add(length)?;
        let crc_end = data_end.checked_add(4)?;
        if crc_end > input.len() {
            return None;
        }
        if ty == b"IHDR" && ihdr.is_none() && length == 13 {
            let d = &input[data_start..data_end];
            ihdr = Some(Ihdr {
                width: u32::from_be_bytes(d[0..4].try_into().ok()?),
                height: u32::from_be_bytes(d[4..8].try_into().ok()?),
                bit_depth: d[8],
                color_type: d[9],
                interlace: d[12],
            });
        }
        if ty == b"IDAT" {
            idat_spans.push(DataSpan {
                data_start,
                data_end,
            });
        }
        if ty == b"IEND" {
            saw_iend = true;
        }
        at = crc_end;
    }
    if !saw_iend || idat_spans.is_empty() {
        return None;
    }
    if at != input.len() {
        return None;
    }
    let ihdr = ihdr?;

    let mut zlib_stream = Vec::new();
    for span in &idat_spans {
        zlib_stream.extend_from_slice(&input[span.data_start..span.data_end]);
    }
    let inflated = zlib_inflate(&zlib_stream, MAX_INFLATED)?;

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
    Some(ParsedPng {
        input,
        idat_spans,
        inflated,
        level,
        ihdr,
    })
}

fn build_envelope(input: &[u8], idat_spans: &[DataSpan]) -> (Vec<u8>, Vec<SpanMeta>, u32) {
    let mut envelope = Vec::with_capacity(input.len());
    let mut cursor = 0usize;
    let mut span_meta: Vec<SpanMeta> = Vec::with_capacity(idat_spans.len());
    for span in idat_spans {
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
    (envelope, span_meta, seg_after_len)
}

/// Pixel byte-stride for filter math. PNG spec §9: filtering uses
/// max(1, bytes_per_pixel) where samples per pixel comes from
/// color_type and the per-sample byte width comes from bit_depth.
/// We only support 8-bit non-interlaced for the unfilter path; mode A
/// covers everything else.
fn bytes_per_pixel(ihdr: &Ihdr) -> Option<usize> {
    if ihdr.bit_depth != 8 || ihdr.interlace != 0 {
        return None;
    }
    let samples = match ihdr.color_type {
        0 => 1, // grayscale
        2 => 3, // RGB
        4 => 2, // grayscale + alpha
        6 => 4, // RGBA
        _ => return None,
    };
    Some(samples)
}

fn decompose_mode_a(parsed: ParsedPng<'_>) -> Option<Decomposition> {
    let (envelope, span_meta, seg_after_len) = build_envelope(parsed.input, &parsed.idat_spans);
    let recipe = encode_recipe_v1(parsed.level, &span_meta, seg_after_len, &envelope);
    Some(Decomposition {
        recipe,
        parts: vec![parsed.inflated],
    })
}

fn decompose_mode_b(parsed: &ParsedPng<'_>) -> Option<Decomposition> {
    let bpp = bytes_per_pixel(&parsed.ihdr)?;
    let w = parsed.ihdr.width as usize;
    let h = parsed.ihdr.height as usize;
    let row_data = w.checked_mul(bpp)?;
    let stride = row_data.checked_add(1)?;
    let expected = stride.checked_mul(h)?;
    if parsed.inflated.len() != expected {
        return None;
    }

    let mut filters = Vec::with_capacity(h);
    let mut raw = Vec::with_capacity(row_data * h);
    let mut prev_raw: Vec<u8> = vec![0u8; row_data];
    for r in 0..h {
        let off = r * stride;
        let filter_type = parsed.inflated[off];
        let filtered = &parsed.inflated[off + 1..off + stride];
        let decoded = unfilter_row(filter_type, filtered, &prev_raw, bpp)?;
        filters.push(filter_type);
        raw.extend_from_slice(&decoded);
        prev_raw = decoded;
    }

    let (envelope, span_meta, seg_after_len) = build_envelope(parsed.input, &parsed.idat_spans);
    let recipe = encode_recipe_v2(
        parsed.level,
        &parsed.ihdr,
        &filters,
        &span_meta,
        seg_after_len,
        &envelope,
    );
    Some(Decomposition {
        recipe,
        parts: vec![raw],
    })
}

fn recompose_mode_a(recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
    let (level, spans, seg_after_len, envelope) = decode_recipe_v1(recipe)?;
    let [inflated] = parts else { return None };
    splice_zlib(&envelope, &spans, seg_after_len, inflated, level)
}

fn recompose_mode_b(recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
    let (level, ihdr, filters, spans, seg_after_len, envelope) = decode_recipe_v2(recipe)?;
    let [raw] = parts else { return None };

    let bpp = bytes_per_pixel(&ihdr)?;
    let w = ihdr.width as usize;
    let h = ihdr.height as usize;
    let row_data = w.checked_mul(bpp)?;
    if raw.len() != row_data * h || filters.len() != h {
        return None;
    }

    let mut inflated = Vec::with_capacity((row_data + 1) * h);
    let mut prev_raw: &[u8] = &[];
    for r in 0..h {
        let row = &raw[r * row_data..(r + 1) * row_data];
        let filter = filters[r];
        let filtered = filter_row(filter, row, prev_raw, bpp)?;
        inflated.push(filter);
        inflated.extend_from_slice(&filtered);
        prev_raw = row;
    }
    splice_zlib(&envelope, &spans, seg_after_len, &inflated, level)
}

fn splice_zlib(
    envelope: &[u8],
    spans: &[SpanMeta],
    seg_after_len: u32,
    inflated: &[u8],
    level: u8,
) -> Option<Vec<u8>> {
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
    for span in spans {
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

fn unfilter_row(filter: u8, filtered: &[u8], prev_raw: &[u8], bpp: usize) -> Option<Vec<u8>> {
    let n = filtered.len();
    if prev_raw.len() != n {
        return None;
    }
    let mut out = vec![0u8; n];
    match filter {
        0 => out.copy_from_slice(filtered),
        1 => {
            for i in 0..n {
                let left = if i >= bpp { out[i - bpp] } else { 0 };
                out[i] = filtered[i].wrapping_add(left);
            }
        }
        2 => {
            for i in 0..n {
                out[i] = filtered[i].wrapping_add(prev_raw[i]);
            }
        }
        3 => {
            for i in 0..n {
                let left = if i >= bpp { out[i - bpp] } else { 0 };
                let up = prev_raw[i];
                let avg = ((left as u16 + up as u16) / 2) as u8;
                out[i] = filtered[i].wrapping_add(avg);
            }
        }
        4 => {
            for i in 0..n {
                let left = if i >= bpp { out[i - bpp] } else { 0 };
                let up = prev_raw[i];
                let up_left = if i >= bpp { prev_raw[i - bpp] } else { 0 };
                out[i] = filtered[i].wrapping_add(paeth(left, up, up_left));
            }
        }
        _ => return None,
    }
    Some(out)
}

fn filter_row(filter: u8, raw: &[u8], prev_raw: &[u8], bpp: usize) -> Option<Vec<u8>> {
    let n = raw.len();
    if !prev_raw.is_empty() && prev_raw.len() != n {
        return None;
    }
    let mut out = vec![0u8; n];
    match filter {
        0 => out.copy_from_slice(raw),
        1 => {
            for i in 0..n {
                let left = if i >= bpp { raw[i - bpp] } else { 0 };
                out[i] = raw[i].wrapping_sub(left);
            }
        }
        2 => {
            for i in 0..n {
                let up = if prev_raw.is_empty() { 0 } else { prev_raw[i] };
                out[i] = raw[i].wrapping_sub(up);
            }
        }
        3 => {
            for i in 0..n {
                let left = if i >= bpp { raw[i - bpp] } else { 0 };
                let up = if prev_raw.is_empty() { 0 } else { prev_raw[i] };
                let avg = ((left as u16 + up as u16) / 2) as u8;
                out[i] = raw[i].wrapping_sub(avg);
            }
        }
        4 => {
            for i in 0..n {
                let left = if i >= bpp { raw[i - bpp] } else { 0 };
                let up = if prev_raw.is_empty() { 0 } else { prev_raw[i] };
                let up_left = if i >= bpp && !prev_raw.is_empty() {
                    prev_raw[i - bpp]
                } else {
                    0
                };
                out[i] = raw[i].wrapping_sub(paeth(left, up, up_left));
            }
        }
        _ => return None,
    }
    Some(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = (a as i32) + (b as i32) - (c as i32);
    let pa = (p - a as i32).unsigned_abs();
    let pb = (p - b as i32).unsigned_abs();
    let pc = (p - c as i32).unsigned_abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
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

fn encode_recipe_v1(level: u8, spans: &[SpanMeta], seg_after_len: u32, envelope: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 1 + 4 + 8 * spans.len() + 4 + 4 + envelope.len());
    out.extend_from_slice(RECIPE_MAGIC_V1);
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

fn decode_recipe_v1(recipe: &[u8]) -> Option<(u8, Vec<SpanMeta>, u32, Vec<u8>)> {
    if recipe.len() < 8 + 1 + 4 || &recipe[0..8] != RECIPE_MAGIC_V1 {
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

#[allow(clippy::too_many_arguments)]
fn encode_recipe_v2(
    level: u8,
    ihdr: &Ihdr,
    filters: &[u8],
    spans: &[SpanMeta],
    seg_after_len: u32,
    envelope: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        8 + 1 + 11 + filters.len() + 4 + 8 * spans.len() + 4 + 4 + envelope.len(),
    );
    out.extend_from_slice(RECIPE_MAGIC_V2);
    out.push(level);
    // IHDR essentials: width, height, bit_depth, color_type, interlace.
    out.extend_from_slice(&ihdr.width.to_le_bytes());
    out.extend_from_slice(&ihdr.height.to_le_bytes());
    out.push(ihdr.bit_depth);
    out.push(ihdr.color_type);
    out.push(ihdr.interlace);
    // Per-row filter codes (length = height).
    out.extend_from_slice(&(filters.len() as u32).to_le_bytes());
    out.extend_from_slice(filters);
    // IDAT spans.
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

#[allow(clippy::type_complexity)]
fn decode_recipe_v2(recipe: &[u8]) -> Option<(u8, Ihdr, Vec<u8>, Vec<SpanMeta>, u32, Vec<u8>)> {
    if recipe.len() < 8 + 1 + 11 + 4 || &recipe[0..8] != RECIPE_MAGIC_V2 {
        return None;
    }
    let level = recipe[8];
    let mut at = 9;
    let width = u32::from_le_bytes(recipe[at..at + 4].try_into().ok()?);
    let height = u32::from_le_bytes(recipe[at + 4..at + 8].try_into().ok()?);
    let bit_depth = recipe[at + 8];
    let color_type = recipe[at + 9];
    let interlace = recipe[at + 10];
    at += 11;
    let ihdr = Ihdr {
        width,
        height,
        bit_depth,
        color_type,
        interlace,
    };

    if at + 4 > recipe.len() {
        return None;
    }
    let fcount = u32::from_le_bytes(recipe[at..at + 4].try_into().ok()?) as usize;
    at += 4;
    if at + fcount > recipe.len() {
        return None;
    }
    let filters = recipe[at..at + fcount].to_vec();
    at += fcount;

    if at + 4 > recipe.len() {
        return None;
    }
    let n = u32::from_le_bytes(recipe[at..at + 4].try_into().ok()?) as usize;
    at += 4;
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
    let envelope = recipe[at..at + env_len].to_vec();
    Some((level, ihdr, filters, spans, seg_after_len, envelope))
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

    /// Build a real, IHDR-correct PNG (8-bit RGB, non-interlaced) the
    /// way Python's zlib / Pillow's manual save_png does. Useful for
    /// exercising the mode B (filter-aware) decompose path.
    fn make_real_rgb_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        assert_eq!(pixels.len(), (width * height * 3) as usize);
        let stride = (width * 3) as usize;
        let mut filtered = Vec::with_capacity((stride + 1) * height as usize);
        for y in 0..height as usize {
            filtered.push(0u8); // filter type 0 (None) — simplest
            filtered.extend_from_slice(&pixels[y * stride..(y + 1) * stride]);
        }
        let idat = zlib_deflate(&filtered, 6).unwrap();

        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIGNATURE);
        // IHDR: w(u32 BE), h(u32 BE), bit_depth=8, color_type=2 (RGB),
        // compression=0, filter_method=0, interlace=0.
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        push_chunk(&mut out, b"IHDR", &ihdr);
        push_chunk(&mut out, b"IDAT", &idat);
        push_chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn mode_b_round_trips_rgb8_png_with_all_zero_filters() {
        // 4x4 RGB image, deterministic pixels
        let pixels: Vec<u8> = (0..4 * 4 * 3)
            .map(|i: i32| (i as u8).wrapping_mul(13))
            .collect();
        let png = make_real_rgb_png(4, 4, &pixels);
        let d = PngPrism.decompose(&png).expect("decompose");
        // Mode B recipe magic
        assert_eq!(&d.recipe[0..8], RECIPE_MAGIC_V2);
        // Single part = raw pixel matrix (no filter bytes)
        assert_eq!(d.parts.len(), 1);
        assert_eq!(d.parts[0], pixels);
        let reco = PngPrism
            .recompose(&d.recipe, &parts_refs(&d.parts))
            .expect("recompose");
        assert_eq!(reco, png);
    }

    /// Two PNGs that share most of their raw pixels (small patch) should
    /// have parts that share most of their bytes — that is the whole
    /// point of mode B. We don't run CDC here, just confirm the parts
    /// only differ where the pixels differ.
    #[test]
    fn mode_b_parts_share_unchanged_pixel_region() {
        let pixels_a: Vec<u8> = (0..16 * 16 * 3).map(|i| i as u8).collect();
        let mut pixels_b = pixels_a.clone();
        // Edit a single pixel at (5, 7): channels [r,g,b]
        let idx = (7 * 16 + 5) * 3;
        pixels_b[idx] = 0xff;
        pixels_b[idx + 1] = 0x00;
        pixels_b[idx + 2] = 0x80;

        let png_a = make_real_rgb_png(16, 16, &pixels_a);
        let png_b = make_real_rgb_png(16, 16, &pixels_b);

        let da = PngPrism.decompose(&png_a).expect("a");
        let db = PngPrism.decompose(&png_b).expect("b");

        // Same length, differ in exactly the edited bytes
        assert_eq!(da.parts[0].len(), db.parts[0].len());
        let differs = da.parts[0]
            .iter()
            .zip(db.parts[0].iter())
            .filter(|(x, y)| x != y)
            .count();
        assert_eq!(
            differs, 3,
            "only the 3 channels of the edited pixel should differ"
        );

        // Avoid the unused-mut clippy lint when both pixel buffers
        // are only read for the byte comparison above.
        let _ = pixels_a.len();
        let _ = pixels_b.len();
    }

    #[test]
    fn mode_b_falls_back_to_mode_a_for_unsupported_color_types() {
        // 16-bit RGB (color_type=2, bit_depth=16): mode B refuses
        // (bytes_per_pixel returns None for bit_depth!=8), so the
        // prism should still accept via mode A.
        let stride = 4usize * 3 * 2; // w=4, 3 channels, 2 bytes each
        let pixels: Vec<u8> = (0..stride * 3).map(|i| i as u8).collect();
        let mut filtered = Vec::with_capacity((stride + 1) * 3);
        for y in 0..3 {
            filtered.push(0u8);
            filtered.extend_from_slice(&pixels[y * stride..(y + 1) * stride]);
        }
        let idat = zlib_deflate(&filtered, 6).unwrap();

        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIGNATURE);
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.extend_from_slice(&3u32.to_be_bytes());
        ihdr.extend_from_slice(&[16, 2, 0, 0, 0]); // 16-bit RGB
        push_chunk(&mut out, b"IHDR", &ihdr);
        push_chunk(&mut out, b"IDAT", &idat);
        push_chunk(&mut out, b"IEND", &[]);

        let d = PngPrism.decompose(&out).expect("16-bit PNG via mode A");
        assert_eq!(&d.recipe[0..8], RECIPE_MAGIC_V1);
        let reco = PngPrism
            .recompose(&d.recipe, &parts_refs(&d.parts))
            .expect("recompose v1");
        assert_eq!(reco, out);
    }
}
