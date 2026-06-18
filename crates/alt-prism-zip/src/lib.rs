//! ZIP container prism — design/prisms.md §2, hot/cold list step 2 after
//! the deflate-strip "universal key" (alt-prism-deflate). Most binary
//! assets people actually review are ZIP-shaped containers (docx, xlsx,
//! pptx, jar, apk, epub, …); cracking the container *and* its members
//! exposes per-member dedup that the bare CDC + bare deflate-strip layer
//! cannot deliver alone.
//!
//! Tier 1 iron law (alt-prism::Registry::decompose_verified): the
//! pipeline accepts only a byte-exact round trip. So we are free to
//! decline as much as we want — any oddity (encryption, ZIP64, data
//! descriptors, archives that producers use to dodge libz's compression
//! decisions) falls back to Tier 0 (verbatim CDC) instead of risking a
//! corrupt recompose.
//!
//! ## Decomposition shape
//!
//! - **parts**: one per entry, in physical-LFH order. For a `stored`
//!   (method 0) entry the part is the body bytes verbatim. For a
//!   `deflate` (method 8) entry the part is the inflated body — that is
//!   what makes member-level dedup actually fire on docx / jar / epub
//!   variants, because the inflated payload is content-stable across
//!   reflows of the outer container.
//! - **recipe**: a header (magic + version + entry count) + a fixed-size
//!   record per entry (method, libz level matched, segment-before-body
//!   length, body length) + a trailing segment-after-last-body length +
//!   the *envelope*: the original archive's bytes with every entry body
//!   cut out. Local file headers, central directory entries, and the
//!   End Of Central Directory record stay verbatim in the envelope, so
//!   all of their crc32 / size / offset fields automatically match the
//!   recomposed body (those bytes never change between rounds — the
//!   round trip Tier 1 verifies guarantees libz reproduces the same
//!   compressed body byte-for-byte).
//!
//! ## Recompose
//!
//! Walk the recipe entries, splicing each segment + body in order. A
//! stored entry copies its part verbatim into the output stream; a
//! deflate entry re-deflates the part with the recorded level via C
//! libz (raw deflate, windowBits = -15). The envelope's central
//! directory / EOCD never need rewriting — bodies must be byte-equal
//! by construction or the prism would have declined at ingest.

use std::io::Read;

use alt_prism::{Decomposition, Prism, PrismId};

/// This prism's stable id in the registry. Don't reuse on retirement.
pub const ZIP_PRISM: PrismId = PrismId(2);

/// Recipe magic — versioned so a future on-disk format bump can switch
/// recompose paths without ambiguity.
const RECIPE_MAGIC: &[u8] = b"ALTZIP01";

/// ZIP signatures (little-endian as the spec writes them).
const LFH_SIG: &[u8] = b"PK\x03\x04";
const CD_SIG: &[u8] = b"PK\x01\x02";
const EOCD_SIG: &[u8] = b"PK\x05\x06";

/// Upper bound on a single inflated member. Adversarial archives can
/// claim multi-GB uncompressed payloads; we never allocate past this.
const MAX_MEMBER_INFLATED: usize = 256 << 20;

/// Upper bound on the whole archive we'll attempt. Larger archives still
/// land at Tier 0, no panic.
const MAX_ARCHIVE: usize = 1 << 31;

/// libz raw-deflate levels tried in producer-likelihood order. python's
/// zipfile defaults to 6, Java's ZipOutputStream uses 8, optimisers use
/// 9, git/zopfli-class outliers may use 1.
const LEVEL_GRID: [i32; 4] = [6, 8, 9, 1];

pub struct ZipPrism;

impl Prism for ZipPrism {
    fn id(&self) -> PrismId {
        ZIP_PRISM
    }

    fn decompose(&self, input: &[u8]) -> Option<Decomposition> {
        if input.len() < 22 || input.len() > MAX_ARCHIVE {
            return None;
        }
        if !input.starts_with(LFH_SIG) {
            return None;
        }
        let eocd = find_eocd(input)?;
        // Parse EOCD record (PK\005\006 + 18 bytes, plus a comment field
        // of variable length we already accounted for in find_eocd).
        if eocd + 22 > input.len() {
            return None;
        }
        let cd_size = u32::from_le_bytes(input[eocd + 12..eocd + 16].try_into().ok()?) as usize;
        let cd_off = u32::from_le_bytes(input[eocd + 16..eocd + 20].try_into().ok()?) as usize;
        if cd_off.checked_add(cd_size)? > input.len() {
            return None;
        }
        // Reject ZIP64 stubs: any of these sentinel values means the
        // real number is in a ZIP64 extra field, which we don't read.
        let total_entries =
            u16::from_le_bytes(input[eocd + 10..eocd + 12].try_into().ok()?) as usize;
        if total_entries == 0xFFFF {
            return None;
        }

        // Walk central directory to collect (lfh_offset, …) for each
        // entry, plus per-entry sanity. Each CD record is at least
        // 46 bytes plus the variable name + extra + comment fields.
        let cd = &input[cd_off..cd_off + cd_size];
        let mut centrals: Vec<CentralEntry> = Vec::with_capacity(total_entries);
        let mut at = 0;
        while at + 46 <= cd.len() {
            if &cd[at..at + 4] != CD_SIG {
                break;
            }
            let flags = u16::from_le_bytes(cd[at + 8..at + 10].try_into().ok()?);
            // Bit 3 = data descriptor (size/crc in trailer, not header):
            // out-of-order data we'd need a different parser for. Decline.
            if flags & 0b1000 != 0 {
                return None;
            }
            // Bit 0 = encrypted: nothing to do at this layer. Decline.
            if flags & 0b1 != 0 {
                return None;
            }
            let method = u16::from_le_bytes(cd[at + 10..at + 12].try_into().ok()?);
            let comp_size = u32::from_le_bytes(cd[at + 20..at + 24].try_into().ok()?);
            let uncomp_size = u32::from_le_bytes(cd[at + 24..at + 28].try_into().ok()?);
            let name_len = u16::from_le_bytes(cd[at + 28..at + 30].try_into().ok()?) as usize;
            let extra_len = u16::from_le_bytes(cd[at + 30..at + 32].try_into().ok()?) as usize;
            let comment_len = u16::from_le_bytes(cd[at + 32..at + 34].try_into().ok()?) as usize;
            let lfh_off = u32::from_le_bytes(cd[at + 42..at + 46].try_into().ok()?);
            // ZIP64 sentinels in the per-entry fields too — decline.
            if comp_size == u32::MAX || uncomp_size == u32::MAX || lfh_off == u32::MAX {
                return None;
            }
            centrals.push(CentralEntry {
                lfh_off: lfh_off as usize,
                method,
                comp_size: comp_size as usize,
            });
            at = at + 46 + name_len + extra_len + comment_len;
        }
        if centrals.len() != total_entries {
            return None;
        }

        // Sort by physical LFH offset so envelope segments come out in
        // file order — the central directory may have a different order,
        // and recompose splices in stream order.
        centrals.sort_by_key(|c| c.lfh_off);
        // Reject overlapping LFH ranges — covers crafted archives where
        // two CD entries point at the same body.
        for w in centrals.windows(2) {
            if w[1].lfh_off < w[0].lfh_off + 30 {
                return None;
            }
        }

        // For each entry parse the LFH and locate its body span. We do
        // *not* trust the LFH's own size fields when they're zero; those
        // mean "see data descriptor", and we already rejected bit-3 above
        // — so the central-directory comp_size is authoritative here.
        let mut entries: Vec<EntryMeta> = Vec::with_capacity(centrals.len());
        let mut parts: Vec<Vec<u8>> = Vec::with_capacity(centrals.len());
        let mut envelope: Vec<u8> = Vec::with_capacity(input.len());
        let mut last_end = 0usize;
        for c in &centrals {
            let lfh = c.lfh_off;
            if lfh + 30 > input.len() {
                return None;
            }
            if &input[lfh..lfh + 4] != LFH_SIG {
                return None;
            }
            let name_len = u16::from_le_bytes(input[lfh + 26..lfh + 28].try_into().ok()?) as usize;
            let extra_len = u16::from_le_bytes(input[lfh + 28..lfh + 30].try_into().ok()?) as usize;
            let body_start = lfh
                .checked_add(30)?
                .checked_add(name_len)?
                .checked_add(extra_len)?;
            let body_end = body_start.checked_add(c.comp_size)?;
            if body_end > input.len() {
                return None;
            }

            // Segment-before-body: bytes from where we left off (after
            // the previous entry's body) up to this entry's body start —
            // that covers this entry's LFH + name + extra and any
            // unstructured bytes between entries.
            let seg_before = &input[last_end..body_start];
            envelope.extend_from_slice(seg_before);

            // Process the body.
            let body = &input[body_start..body_end];
            let (level, part_bytes) = match c.method {
                0 => (0xFFu8, body.to_vec()),
                8 => {
                    let inflated = raw_inflate(body, MAX_MEMBER_INFLATED)?;
                    let mut matched: Option<u8> = None;
                    for level in LEVEL_GRID {
                        if let Some(reco) = raw_deflate(&inflated, level)
                            && reco.len() == body.len()
                            && reco.as_slice() == body
                        {
                            matched = Some(level as u8);
                            break;
                        }
                    }
                    // Tier 1 iron law: if libz can't reproduce the body
                    // bit-exactly, fall back to Tier 0 for the whole
                    // archive (we never store a "close enough" delta).
                    (matched?, inflated)
                }
                _ => return None,
            };

            entries.push(EntryMeta {
                method: c.method,
                level,
                seg_before_len: seg_before.len() as u32,
                body_len: c.comp_size as u32,
            });
            parts.push(part_bytes);
            last_end = body_end;
        }

        // Trailing segment: central directory + EOCD + any archive
        // comment (and whatever else sits past the final body).
        let trailing = &input[last_end..];
        envelope.extend_from_slice(trailing);
        let seg_after_len = trailing.len() as u32;

        let recipe = encode_recipe(&entries, seg_after_len, &envelope);
        Some(Decomposition { recipe, parts })
    }

    fn recompose(&self, recipe: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
        let (entries, seg_after_len, envelope) = decode_recipe(recipe)?;
        if entries.len() != parts.len() {
            return None;
        }
        let total_body_len: u64 = entries.iter().map(|e| e.body_len as u64).sum();
        let envelope_total: u64 =
            entries.iter().map(|e| e.seg_before_len as u64).sum::<u64>() + seg_after_len as u64;
        if envelope.len() as u64 != envelope_total {
            return None;
        }
        let mut out = Vec::with_capacity(envelope.len() + total_body_len as usize);
        let mut env_cursor = 0usize;
        for (entry, part) in entries.iter().zip(parts.iter()) {
            let seg_len = entry.seg_before_len as usize;
            let next = env_cursor.checked_add(seg_len)?;
            if next > envelope.len() {
                return None;
            }
            out.extend_from_slice(&envelope[env_cursor..next]);
            env_cursor = next;

            let body = match entry.method {
                0 => {
                    if part.len() as u32 != entry.body_len {
                        return None;
                    }
                    part.to_vec()
                }
                8 => {
                    if entry.level == 0xFF {
                        return None;
                    }
                    let reco = raw_deflate(part, entry.level as i32)?;
                    if reco.len() as u32 != entry.body_len {
                        return None;
                    }
                    reco
                }
                _ => return None,
            };
            out.extend_from_slice(&body);
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
struct CentralEntry {
    lfh_off: usize,
    method: u16,
    comp_size: usize,
}

#[derive(Debug, Clone, Copy)]
struct EntryMeta {
    method: u16,
    level: u8,
    seg_before_len: u32,
    body_len: u32,
}

fn encode_recipe(entries: &[EntryMeta], seg_after_len: u32, envelope: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 4 + 11 * entries.len() + 4 + 4 + envelope.len());
    out.extend_from_slice(RECIPE_MAGIC);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        out.extend_from_slice(&e.method.to_le_bytes());
        out.push(e.level);
        out.extend_from_slice(&e.seg_before_len.to_le_bytes());
        out.extend_from_slice(&e.body_len.to_le_bytes());
    }
    out.extend_from_slice(&seg_after_len.to_le_bytes());
    out.extend_from_slice(&(envelope.len() as u32).to_le_bytes());
    out.extend_from_slice(envelope);
    out
}

fn decode_recipe(recipe: &[u8]) -> Option<(Vec<EntryMeta>, u32, Vec<u8>)> {
    if recipe.len() < 8 + 4 || &recipe[0..8] != RECIPE_MAGIC {
        return None;
    }
    let n = u32::from_le_bytes(recipe[8..12].try_into().ok()?) as usize;
    let mut at = 12;
    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        if at + 11 > recipe.len() {
            return None;
        }
        let method = u16::from_le_bytes(recipe[at..at + 2].try_into().ok()?);
        let level = recipe[at + 2];
        let seg_before_len = u32::from_le_bytes(recipe[at + 3..at + 7].try_into().ok()?);
        let body_len = u32::from_le_bytes(recipe[at + 7..at + 11].try_into().ok()?);
        entries.push(EntryMeta {
            method,
            level,
            seg_before_len,
            body_len,
        });
        at += 11;
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
    Some((entries, seg_after_len, envelope))
}

fn find_eocd(data: &[u8]) -> Option<usize> {
    // EOCD's variable archive-comment is up to 65535 bytes; scan back.
    let max_scan = 65557.min(data.len());
    let start = data.len() - max_scan;
    for i in (start..=data.len().saturating_sub(4)).rev() {
        if &data[i..i + 4] == EOCD_SIG {
            return Some(i);
        }
    }
    None
}

fn raw_inflate(input: &[u8], max: usize) -> Option<Vec<u8>> {
    // Empty member body is legal — a zero-byte "stored" inside a deflate
    // stream produces an empty inflated payload (flate2 wants a valid
    // deflate stream though; an empty input would error). Short-circuit.
    if input.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(input)
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .ok()?;
    if out.len() > max {
        return None;
    }
    Some(out)
}

/// libz's alloc shim. `deflateInit2_` rejects NULL function pointers
/// the Rust type system anyway can't express; we route alloc/free
/// through libc (the same allocator zlib's default uses), so behaviour
/// matches `Z_NULL` exactly without the null-fn-pointer UB.
unsafe extern "C" fn alt_zalloc(
    _opaque: libz_sys::voidpf,
    items: libz_sys::uInt,
    size: libz_sys::uInt,
) -> libz_sys::voidpf {
    let total = (items as usize).saturating_mul(size as usize);
    unsafe { libc::malloc(total) as libz_sys::voidpf }
}

unsafe extern "C" fn alt_zfree(_opaque: libz_sys::voidpf, address: libz_sys::voidpf) {
    unsafe { libc::free(address as *mut _) }
}

fn raw_deflate(data: &[u8], level: i32) -> Option<Vec<u8>> {
    if !(0..=9).contains(&level) {
        return None;
    }
    unsafe {
        let mut strm = libz_sys::z_stream {
            next_in: std::ptr::null_mut(),
            avail_in: 0,
            total_in: 0,
            next_out: std::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: std::ptr::null_mut(),
            state: std::ptr::null_mut(),
            zalloc: alt_zalloc,
            zfree: alt_zfree,
            opaque: std::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        };
        let version = libz_sys::zlibVersion();
        let rc = libz_sys::deflateInit2_(
            &mut strm,
            level,
            libz_sys::Z_DEFLATED,
            -15, // raw deflate, no zlib header / trailer
            8,   // memLevel default
            libz_sys::Z_DEFAULT_STRATEGY,
            version,
            std::mem::size_of::<libz_sys::z_stream>() as i32,
        );
        if rc != libz_sys::Z_OK {
            return None;
        }
        let bound = libz_sys::deflateBound(&mut strm, data.len() as libz_sys::uLong) as usize;
        let mut out = vec![0u8; bound.max(1)];
        strm.next_in = data.as_ptr() as *mut _;
        strm.avail_in = data.len() as libz_sys::uInt;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as libz_sys::uInt;
        let rc = libz_sys::deflate(&mut strm, libz_sys::Z_FINISH);
        if rc != libz_sys::Z_STREAM_END {
            libz_sys::deflateEnd(&mut strm);
            return None;
        }
        let total_out = strm.total_out as usize;
        libz_sys::deflateEnd(&mut strm);
        out.truncate(total_out);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a deflate-compressed ZIP entry the way python's stdlib
    /// (or any libz-based writer) does so we know the libz round trip
    /// matches.
    fn make_zip_with(entries: &[(&str, &[u8], u8 /*level*/)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd_entries: Vec<(u32, &str, u32, u32, u32, u16)> = Vec::new();
        for (name, body, level) in entries {
            let lfh_off = out.len() as u32;
            let compressed = raw_deflate(body, *level as i32).unwrap();
            let crc = crc32_naive(body);
            // LFH
            out.extend_from_slice(LFH_SIG);
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&8u16.to_le_bytes()); // method = deflate
            out.extend_from_slice(&0u32.to_le_bytes()); // mod time + date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra_len
            out.write_all(name.as_bytes()).unwrap();
            out.extend_from_slice(&compressed);
            cd_entries.push((
                lfh_off,
                name,
                crc,
                compressed.len() as u32,
                body.len() as u32,
                8,
            ));
        }
        let cd_off = out.len() as u32;
        for (lfh_off, name, crc, comp_size, uncomp_size, method) in &cd_entries {
            out.extend_from_slice(CD_SIG);
            out.extend_from_slice(&20u16.to_le_bytes()); // version made by
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // mod time + date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&comp_size.to_le_bytes());
            out.extend_from_slice(&uncomp_size.to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra_len
            out.extend_from_slice(&0u16.to_le_bytes()); // comment_len
            out.extend_from_slice(&0u16.to_le_bytes()); // disk start
            out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            out.extend_from_slice(&lfh_off.to_le_bytes());
            out.write_all(name.as_bytes()).unwrap();
        }
        let cd_size = (out.len() as u32) - cd_off;
        // EOCD
        out.extend_from_slice(EOCD_SIG);
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // disk with CD
        out.extend_from_slice(&(cd_entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd_entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment_len
        out
    }

    /// IEEE 802.3 CRC32 (zip's polynomial). Small enough to inline so
    /// tests don't depend on a hash crate.
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

    #[test]
    fn round_trips_simple_two_entry_zip_at_level_6() {
        let zip = make_zip_with(&[
            ("a.txt", b"hello hello hello world", 6),
            ("b.txt", b"different content for the second entry", 6),
        ]);
        let d = ZipPrism.decompose(&zip).expect("zip should decompose");
        assert_eq!(d.parts.len(), 2);
        assert_eq!(d.parts[0], b"hello hello hello world");
        let recomposed = ZipPrism
            .recompose(
                &d.recipe,
                &d.parts.iter().map(|v| v.as_slice()).collect::<Vec<_>>(),
            )
            .expect("zip should recompose");
        assert_eq!(recomposed, zip);
    }

    #[test]
    fn round_trips_at_level_8_and_9_used_by_jar_and_optimisers() {
        for level in [8, 9] {
            let zip = make_zip_with(&[("only.bin", &(0..255u8).collect::<Vec<u8>>(), level)]);
            let d = ZipPrism.decompose(&zip).expect("decompose");
            let recomposed = ZipPrism
                .recompose(
                    &d.recipe,
                    &d.parts.iter().map(|v| v.as_slice()).collect::<Vec<_>>(),
                )
                .expect("recompose");
            assert_eq!(recomposed, zip);
        }
    }

    #[test]
    fn declines_non_zip_input_quickly() {
        assert!(ZipPrism.decompose(b"not a zip at all").is_none());
        assert!(ZipPrism.decompose(&[]).is_none());
        // PNG signature
        assert!(ZipPrism.decompose(b"\x89PNG\r\n\x1a\n").is_none());
    }

    #[test]
    fn declines_corrupt_archive_rather_than_panicking() {
        let mut zip = make_zip_with(&[("x.txt", b"hello", 6)]);
        // Truncate halfway through the body — must not panic.
        zip.truncate(20);
        assert!(ZipPrism.decompose(&zip).is_none());
    }

    #[test]
    fn recompose_rejects_wrong_part_count() {
        let zip = make_zip_with(&[("a", b"hello", 6), ("b", b"world", 6)]);
        let d = ZipPrism.decompose(&zip).expect("decompose");
        // Only one part — should refuse to rebuild.
        let one_part = vec![d.parts[0].as_slice()];
        assert!(ZipPrism.recompose(&d.recipe, &one_part).is_none());
    }
}
