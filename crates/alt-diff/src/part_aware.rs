//! M10/W20 — B2 part-aware diff.
//!
//! Where [`crate::binary::chunk_diff`] (B1) tells you "47% of bytes are
//! shared" for any binary, this module gives you "IDAT chunks changed
//! but IHDR didn't" when both sides decompose under a known format —
//! the answer a reviewer actually asks of a binary diff.
//!
//! The detection set is intentionally narrow at W20: PNG only. The
//! shape ([`Summary`] / [`PartChange`]) is format-agnostic so adding
//! ZIP / PDF / image kinds later is a `match` arm in [`summary`],
//! never a redesign of the surface.
//!
//! Design parity with [`crate::perceptual`]: never panic on adversarial
//! input, return `None` on any structural problem so the caller
//! degrades to the chunk-diff summary alone.

use std::collections::BTreeMap;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const ZIP_LFH_SIGNATURE: &[u8] = b"PK\x03\x04";
const ZIP_CD_SIGNATURE: &[u8] = b"PK\x01\x02";
const ZIP_EOCD_SIGNATURE: &[u8] = b"PK\x05\x06";

/// What kind of file we recognised for part-aware decomposition. Mirrors
/// [`crate::perceptual::ContentKind`] but is its own enum because the
/// part-aware set may diverge from the perceptual set (a future format
/// might be perceptible without being chunked, or vice-versa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Png,
    /// ZIP container: covers .zip, .jar, .apk, .epub, and the entire
    /// OOXML family (.docx / .xlsx / .pptx) since OOXML files are ZIPs.
    /// M12/W32.
    Zip,
}

impl PartKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PartKind::Png => "png",
            PartKind::Zip => "zip",
        }
    }
}

/// One named part's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartChange {
    /// The part is byte-identical on both sides — nothing to review.
    Same,
    /// Present on both sides but bytes differ. Carries the old / new
    /// total body byte counts so the caller can render `X → Y bytes`.
    Changed { old_bytes: usize, new_bytes: usize },
    /// Only on the new side (new chunk type, or extra occurrence).
    Added { new_bytes: usize },
    /// Only on the old side.
    Removed { old_bytes: usize },
}

impl PartChange {
    fn is_same(&self) -> bool {
        matches!(self, PartChange::Same)
    }
}

/// One file's part-aware diff. Always non-empty when returned; empty
/// would be redundant with the file-level "no change" line the
/// caller already emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub kind: PartKind,
    /// Each named part the format defines, in stable lexical order so
    /// the rendered line is deterministic across runs. The name is the
    /// format-specific identifier (PNG chunk type, etc.).
    pub parts: Vec<(String, PartChange)>,
}

impl Summary {
    /// Render the summary as a single human line, in the same
    /// information-dense style as the B1 chunk-diff line so they sit
    /// nicely beside each other in `alt diff`. Parts that didn't
    /// change are dropped from the line so the signal-to-noise stays
    /// high — the absence of a part name in the output means it's
    /// unchanged (or wasn't present on either side).
    pub fn render(&self) -> String {
        let mut bits: Vec<String> = Vec::new();
        for (name, change) in &self.parts {
            match change {
                PartChange::Same => {}
                PartChange::Changed {
                    old_bytes,
                    new_bytes,
                } => {
                    bits.push(format!("{name} {old_bytes}→{new_bytes} B"));
                }
                PartChange::Added { new_bytes } => {
                    bits.push(format!("{name} added ({new_bytes} B)"));
                }
                PartChange::Removed { old_bytes } => {
                    bits.push(format!("{name} removed ({old_bytes} B)"));
                }
            }
        }
        if bits.is_empty() {
            return format!("{}: parts unchanged", self.kind.as_str());
        }
        format!("{}: {}", self.kind.as_str(), bits.join(" | "))
    }

    /// `true` iff every part is `Same`. Lets the caller skip emitting
    /// the part-aware line when it would just repeat "all chunks the
    /// same" — useful when the two PNGs share metadata exactly and
    /// only the IDAT differs (or vice-versa, all metadata changed and
    /// the IDAT is identical).
    pub fn all_same(&self) -> bool {
        self.parts.iter().all(|(_, c)| c.is_same())
    }
}

/// Top-level part-aware diff. Returns `None` when the two inputs aren't
/// both a recognised, parseable kind — degrading gracefully to the
/// caller's B1 chunk-diff line.
pub fn summary(old: &[u8], new: &[u8]) -> Option<Summary> {
    if old.starts_with(PNG_SIGNATURE) && new.starts_with(PNG_SIGNATURE) {
        return png_summary(old, new);
    }
    if old.starts_with(ZIP_LFH_SIGNATURE) && new.starts_with(ZIP_LFH_SIGNATURE) {
        return zip_summary(old, new);
    }
    None
}

fn png_summary(old: &[u8], new: &[u8]) -> Option<Summary> {
    let old_chunks = png_chunks(old)?;
    let new_chunks = png_chunks(new)?;
    Some(part_summary(PartKind::Png, &old_chunks, &new_chunks))
}

fn zip_summary(old: &[u8], new: &[u8]) -> Option<Summary> {
    let old_entries = zip_entries(old)?;
    let new_entries = zip_entries(new)?;
    Some(part_summary(PartKind::Zip, &old_entries, &new_entries))
}

/// Parse ZIP central directory and bucket each entry by file name.
/// The `body` field of each `ChunkAccum` is filled with the entry's
/// `(crc32, compressed_size)` 8-byte tuple — two ZIP entries with the
/// same name compare equal iff *both* match. CRC32 alone has a
/// collision probability of 1/2^32; combined with the compressed
/// size that's far below the noise floor of "two ZIPs claiming the
/// same content".
///
/// We don't actually decompress any entry: this is a `O(N entries)`
/// pass over the central directory only, so a 10K-entry archive
/// still diffs in well under a millisecond.
///
/// Returns `None` (fall back to B1) on any structural problem —
/// truncated EOCD, malformed CD entry, oversized name length — so a
/// corrupt or encrypted-by-trick ZIP degrades gracefully instead of
/// crashing the diff path.
fn zip_entries(data: &[u8]) -> Option<BTreeMap<String, ChunkAccum>> {
    let eocd = find_eocd(data)?;
    if eocd + 22 > data.len() {
        return None;
    }
    // EOCD layout we care about:
    //   +0  PK\005\006
    //   +10 total CD entries (u16 LE)
    //   +12 size of CD (u32 LE)
    //   +16 offset of CD (u32 LE)
    let cd_size = u32::from_le_bytes(data[eocd + 12..eocd + 16].try_into().ok()?) as usize;
    let cd_off = u32::from_le_bytes(data[eocd + 16..eocd + 20].try_into().ok()?) as usize;
    if cd_off.checked_add(cd_size)? > data.len() {
        return None;
    }

    let cd = &data[cd_off..cd_off + cd_size];
    let mut at = 0;
    let mut out: BTreeMap<String, ChunkAccum> = BTreeMap::new();
    while at + 46 <= cd.len() {
        if &cd[at..at + 4] != ZIP_CD_SIGNATURE {
            break;
        }
        // Central directory header layout (the bytes we read):
        //   +16 crc32 (u32 LE)
        //   +20 compressed size (u32 LE)
        //   +24 uncompressed size (u32 LE)
        //   +28 file name length (u16 LE)
        //   +30 extra field length (u16 LE)
        //   +32 file comment length (u16 LE)
        //   +46 file name (variable)
        let crc32 = u32::from_le_bytes(cd[at + 16..at + 20].try_into().ok()?);
        let comp_size = u32::from_le_bytes(cd[at + 20..at + 24].try_into().ok()?);
        let name_len = u16::from_le_bytes(cd[at + 28..at + 30].try_into().ok()?) as usize;
        let extra_len = u16::from_le_bytes(cd[at + 30..at + 32].try_into().ok()?) as usize;
        let comment_len = u16::from_le_bytes(cd[at + 32..at + 34].try_into().ok()?) as usize;
        let name_start = at + 46;
        let name_end = name_start.checked_add(name_len)?;
        if name_end > cd.len() {
            return None;
        }
        let name = String::from_utf8_lossy(&cd[name_start..name_end]).into_owned();
        let entry = out.entry(name).or_default();
        // Same-bytes proof = (crc32, compressed_size) tuple. We push
        // both into the accumulator's body field so `part_summary`'s
        // bytewise `o.body == n.body` check picks up either side
        // changing.
        entry.body.extend_from_slice(&crc32.to_le_bytes());
        entry.body.extend_from_slice(&comp_size.to_le_bytes());
        entry.bytes += comp_size as usize;
        entry.count += 1;
        at = name_end + extra_len + comment_len;
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Walk backwards from the end of the file looking for the End Of
/// Central Directory record (`PK\005\006`). The EOCD record may be
/// followed by an arbitrary ZIP file comment of up to 65535 bytes,
/// so we scan the last 65557 bytes (22 EOCD + 65535 comment) at most.
fn find_eocd(data: &[u8]) -> Option<usize> {
    let max_scan = 65557.min(data.len());
    let start = data.len() - max_scan;
    // Search from the end, biased toward the first EOCD encountered
    // (the legit one). A pathological file with the EOCD signature
    // embedded in its comment would be picked up first by reverse
    // scan; that's the same heuristic real ZIP tools use.
    for i in (start..=data.len().saturating_sub(4)).rev() {
        if &data[i..i + 4] == ZIP_EOCD_SIGNATURE {
            return Some(i);
        }
    }
    None
}

/// Collect all chunks in a PNG, indexed by 4-byte type. Multiple
/// occurrences of the same type (IDAT especially) concatenate into
/// one entry — that's the actual review unit for PNG (a single IDAT
/// payload is meaningless as a unit; the assembled IDAT stream is
/// what encodes pixels).
fn png_chunks(data: &[u8]) -> Option<BTreeMap<String, ChunkAccum>> {
    if !data.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut at = PNG_SIGNATURE.len();
    let mut out: BTreeMap<String, ChunkAccum> = BTreeMap::new();
    while at + 8 <= data.len() {
        let len = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
        let kind_bytes: [u8; 4] = data[at + 4..at + 8].try_into().ok()?;
        let body_start = at + 8;
        let body_end = body_start.checked_add(len)?;
        if body_end + 4 > data.len() {
            return None;
        }
        let key = String::from_utf8_lossy(&kind_bytes).into_owned();
        let entry = out.entry(key).or_default();
        entry.bytes += len;
        // Hash the body bytes into the accumulator so the diff can tell
        // "same bytes, same byte count" apart from "different bytes,
        // coincidentally same byte count" — the latter is real (two
        // different IHDRs are both 13 bytes).
        entry.body.extend_from_slice(&data[body_start..body_end]);
        entry.count += 1;
        at = body_end + 4;
        if &kind_bytes == b"IEND" {
            break;
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

#[derive(Debug, Default)]
struct ChunkAccum {
    bytes: usize,
    body: Vec<u8>,
    #[allow(dead_code)]
    count: usize,
}

fn part_summary(
    kind: PartKind,
    old: &BTreeMap<String, ChunkAccum>,
    new: &BTreeMap<String, ChunkAccum>,
) -> Summary {
    let mut all_keys: Vec<String> = old.keys().chain(new.keys()).cloned().collect();
    all_keys.sort();
    all_keys.dedup();
    let mut parts = Vec::new();
    for name in all_keys {
        let change = match (old.get(&name), new.get(&name)) {
            (Some(o), Some(n)) => {
                if o.body == n.body {
                    PartChange::Same
                } else {
                    PartChange::Changed {
                        old_bytes: o.bytes,
                        new_bytes: n.bytes,
                    }
                }
            }
            (None, Some(n)) => PartChange::Added { new_bytes: n.bytes },
            (Some(o), None) => PartChange::Removed { old_bytes: o.bytes },
            (None, None) => unreachable!(),
        };
        parts.push((name, change));
    }
    Summary { kind, parts }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write one PNG chunk as `<len:4><type:4><body><crc:4 zero>`.
    /// CRC is a placeholder — our chunk walker ignores it (B2 is a
    /// diff helper, not a PNG validator).
    fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&[0, 0, 0, 0]);
    }

    /// Synthesise a minimal PNG: signature + IHDR + one IDAT containing
    /// just `pixel` (uncompressed, structurally invalid as a real PNG
    /// but byte-perfect for the part-aware chunk walker) + optional
    /// tEXt comment + IEND.
    fn synth_png(pixel: u8, comment: Option<&str>) -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        // IHDR: 13 bytes. We use the same 13-byte body regardless of
        // pixel so IHDR stays byte-equal and the diff surfaces IDAT
        // alone.
        let ihdr = [0u8; 13];
        write_chunk(&mut out, b"IHDR", &ihdr);
        write_chunk(&mut out, b"IDAT", &[pixel]);
        if let Some(c) = comment {
            let mut body = b"Comment\0".to_vec();
            body.extend_from_slice(c.as_bytes());
            write_chunk(&mut out, b"tEXt", &body);
        }
        write_chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn same_png_yields_all_same() {
        let a = synth_png(1, None);
        let s = summary(&a, &a).expect("png recognised");
        assert!(s.all_same(), "identical inputs must be all-same");
        assert_eq!(s.render(), "png: parts unchanged");
    }

    #[test]
    fn idat_change_pixel_renders_idat_changed() {
        let a = synth_png(1, None);
        let b = synth_png(2, None);
        let s = summary(&a, &b).expect("png recognised");
        assert!(!s.all_same());
        let rendered = s.render();
        assert!(
            rendered.contains("IDAT"),
            "pixel change must surface as IDAT: {rendered}"
        );
        assert!(
            !rendered.contains("IHDR"),
            "IHDR is bit-identical for two same-size pngs: {rendered}"
        );
    }

    #[test]
    fn added_text_chunk_renders_added() {
        let a = synth_png(1, None);
        let b = synth_png(1, Some("hello"));
        let s = summary(&a, &b).expect("png recognised");
        let rendered = s.render();
        assert!(
            rendered.contains("tEXt added"),
            "tEXt insertion must render as added: {rendered}"
        );
    }

    #[test]
    fn non_png_inputs_return_none() {
        let raw = b"not a png".to_vec();
        assert!(summary(&raw, &raw).is_none());
    }

    #[test]
    fn one_side_non_png_returns_none() {
        let a = synth_png(1, None);
        let b = b"not a png".to_vec();
        assert!(summary(&a, &b).is_none());
    }

    /// Build a structurally-valid (bare-bones) ZIP with `entries =
    /// [(name, crc32, compressed_size)]`. We don't write real file
    /// data — the central directory is the only thing the part-aware
    /// pass reads, so synthesising LFH + CD + EOCD with placeholder
    /// bodies is sufficient. CRC and sizes go into the CD entry as the
    /// dedup key.
    fn synth_zip(entries: &[(&str, u32, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, crc, csize) in entries {
            // Local File Header — we keep it short and the body
            // placeholder identical-length to compressed size so the
            // LFH region's bytes accurately match `compressed_size`.
            // The walker doesn't actually scan LFH for the diff, so
            // the placeholder bytes' content doesn't matter.
            let lfh_off = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04"); // signature
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version/flags/method/time/date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&csize.to_le_bytes()); // compressed
            out.extend_from_slice(&csize.to_le_bytes()); // uncompressed
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra
            out.extend_from_slice(name.as_bytes());
            // Body placeholder so subsequent entries don't overlap.
            out.extend(std::iter::repeat_n(0u8, *csize as usize));

            // Central Directory entry — this is what zip_summary
            // actually parses.
            cd.extend_from_slice(b"PK\x01\x02"); // signature
            cd.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // ver/ver/flag/method/time/date
            cd.extend_from_slice(&crc.to_le_bytes());
            cd.extend_from_slice(&csize.to_le_bytes()); // compressed
            cd.extend_from_slice(&csize.to_le_bytes()); // uncompressed
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes()); // extra
            cd.extend_from_slice(&0u16.to_le_bytes()); // comment
            cd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // disk/internal/external attrs
            cd.extend_from_slice(&lfh_off.to_le_bytes());
            cd.extend_from_slice(name.as_bytes());
        }

        let cd_off = out.len() as u32;
        let cd_size = cd.len() as u32;
        out.extend_from_slice(&cd);

        // End of Central Directory.
        out.extend_from_slice(b"PK\x05\x06"); // signature
        out.extend_from_slice(&[0, 0, 0, 0]); // disk numbers
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes()); // entries this disk
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes()); // entries total
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        out
    }

    #[test]
    fn zip_signature_recognised_and_same_zip_yields_all_same() {
        let z = synth_zip(&[("a.txt", 0xDEAD_BEEF, 4), ("b.txt", 0xCAFE_BABE, 7)]);
        let s = summary(&z, &z).expect("zip recognised");
        assert_eq!(s.kind, PartKind::Zip);
        assert!(s.all_same(), "identical zips must be all-same");
        assert_eq!(s.render(), "zip: parts unchanged");
    }

    #[test]
    fn zip_changed_entry_renders_changed() {
        // word/document.xml differs by crc/size; content_types.xml is
        // bit-identical. Mirrors the OOXML "edit one document part"
        // shape end users actually push through alt diff.
        let a = synth_zip(&[
            ("[Content_Types].xml", 0x1111_1111, 64),
            ("word/document.xml", 0x2222_2222, 1024),
        ]);
        let b = synth_zip(&[
            ("[Content_Types].xml", 0x1111_1111, 64),
            ("word/document.xml", 0x3333_3333, 1050),
        ]);
        let s = summary(&a, &b).expect("zip recognised");
        let rendered = s.render();
        assert!(
            rendered.contains("word/document.xml"),
            "changed entry must surface: {rendered}"
        );
        assert!(
            !rendered.contains("[Content_Types].xml"),
            "unchanged entry must be dropped from the line: {rendered}"
        );
    }

    #[test]
    fn zip_added_entry_renders_added() {
        let a = synth_zip(&[("a.txt", 0xAAAA_AAAA, 16)]);
        let b = synth_zip(&[
            ("a.txt", 0xAAAA_AAAA, 16),
            ("new/file.bin", 0xBBBB_BBBB, 32),
        ]);
        let rendered = summary(&a, &b).unwrap().render();
        assert!(
            rendered.contains("new/file.bin added"),
            "added entry must render as added: {rendered}"
        );
    }

    #[test]
    fn zip_removed_entry_renders_removed() {
        let a = synth_zip(&[
            ("a.txt", 0xAAAA_AAAA, 16),
            ("old/dead.bin", 0xDEAD_DEAD, 64),
        ]);
        let b = synth_zip(&[("a.txt", 0xAAAA_AAAA, 16)]);
        let rendered = summary(&a, &b).unwrap().render();
        assert!(
            rendered.contains("old/dead.bin removed"),
            "removed entry must render as removed: {rendered}"
        );
    }

    #[test]
    fn corrupt_zip_falls_back_silently() {
        // Truncated EOCD = no comma-separated CD location to parse;
        // zip_summary must return None so the caller drops to the B1
        // chunk-diff line instead of panicking.
        let mut z = synth_zip(&[("a.txt", 0x1, 4)]);
        z.truncate(z.len() - 10);
        let r = summary(&z, &z);
        assert!(r.is_none(), "truncated ZIP must fall back, not panic");
    }
}
