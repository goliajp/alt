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

/// What kind of file we recognised for part-aware decomposition. Mirrors
/// [`crate::perceptual::ContentKind`] but is its own enum because the
/// part-aware set may diverge from the perceptual set (a future format
/// might be perceptible without being chunked, or vice-versa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Png,
}

impl PartKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PartKind::Png => "png",
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
    None
}

fn png_summary(old: &[u8], new: &[u8]) -> Option<Summary> {
    let old_chunks = png_chunks(old)?;
    let new_chunks = png_chunks(new)?;
    Some(part_summary(PartKind::Png, &old_chunks, &new_chunks))
}

/// Collect all chunks in a PNG, indexed by 4-byte type. Multiple
/// occurrences of the same type (IDAT especially) concatenate into
/// one entry — that's the actual review unit for PNG (a single IDAT
/// payload is meaningless as a unit; the assembled IDAT stream is
/// what encodes pixels).
fn png_chunks(data: &[u8]) -> Option<BTreeMap<[u8; 4], ChunkAccum>> {
    if !data.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut at = PNG_SIGNATURE.len();
    let mut out: BTreeMap<[u8; 4], ChunkAccum> = BTreeMap::new();
    while at + 8 <= data.len() {
        let len = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
        let kind_bytes: [u8; 4] = data[at + 4..at + 8].try_into().ok()?;
        let body_start = at + 8;
        let body_end = body_start.checked_add(len)?;
        if body_end + 4 > data.len() {
            return None;
        }
        let entry = out.entry(kind_bytes).or_default();
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
    old: &BTreeMap<[u8; 4], ChunkAccum>,
    new: &BTreeMap<[u8; 4], ChunkAccum>,
) -> Summary {
    let mut all_kinds: Vec<[u8; 4]> = old.keys().chain(new.keys()).copied().collect();
    all_kinds.sort();
    all_kinds.dedup();
    let mut parts = Vec::new();
    for k in all_kinds {
        let name = String::from_utf8_lossy(&k).into_owned();
        let change = match (old.get(&k), new.get(&k)) {
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
}
