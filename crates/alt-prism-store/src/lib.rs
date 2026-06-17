//! Tier 1 storage: a domain layer over the (Tier 0) blob store that applies
//! the prism pipeline at write time. The alt-store crate stays pure — it
//! knows nothing about prisms; this layer owns the registry and the Tier 1
//! bookkeeping.
//!
//! On `put`, the registry's verify-before-store pipeline (alt-prism) either
//! accepts a byte-exact decomposition or declines. Accepted: each part is
//! stored as a normal blob (so parts dedup against each other and across
//! files via CDC — the whole point), a small recipe record is stored as a
//! blob too, and a fixed-size map records `original blob id → record blob
//! id`. Declined: the bytes are stored Tier 0 verbatim. The blob id is
//! always BLAKE3 of the original content, unchanged by the storage form
//! (native-store §1 / VISION 信条 3).
//!
//! `get` recomposes a Tier 1 blob from its parts and re-hashes the result
//! against the requested id — the integrity boundary, so a corrupt part or
//! recipe surfaces, never wrong bytes.

use std::path::Path;

use alt_prism::{PrismId, Registry};
use alt_store::{BlobId, BlobStore, StoreError};

mod tier1map;
use tier1map::Tier1Map;

#[derive(Debug, thiserror::Error)]
pub enum PrismStoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("tier1 record corrupt: {0}")]
    Record(&'static str),
    #[error("tier1 recompose failed for {0}")]
    Recompose(BlobId),
    #[error("recomposed bytes do not hash to {0}")]
    HashMismatch(BlobId),
}

/// A blob store that decomposes recognized formats into deduplicated parts
/// (Tier 1) and falls back to verbatim storage (Tier 0) otherwise.
pub struct PrismStore {
    blobs: BlobStore,
    tier1: Tier1Map,
    registry: Registry,
}

impl PrismStore {
    /// Opens a prism store rooted at `dir` with the given prism registry.
    pub fn open(dir: &Path, registry: Registry) -> Result<Self, PrismStoreError> {
        let blobs = BlobStore::open(dir.join("store"))?;
        let tier1 = Tier1Map::open(dir)?;
        Ok(Self {
            blobs,
            tier1,
            registry,
        })
    }

    /// Stores `data`, deduplicating by content. Returns its blob id (BLAKE3
    /// of `data`) regardless of which tier it lands in.
    pub fn put(&mut self, data: &[u8]) -> Result<BlobId, PrismStoreError> {
        let id = BlobId::of(data);
        if self.tier1.contains(id) || self.blobs.contains(id) {
            return Ok(id); // already stored, either tier
        }
        match self.registry.decompose_verified(data) {
            Some(t1) => {
                let mut part_ids = Vec::with_capacity(t1.decomposition.parts.len());
                for part in &t1.decomposition.parts {
                    part_ids.push(self.blobs.put(part)?);
                }
                let record = encode_record(t1.prism, &t1.decomposition.recipe, &part_ids);
                let record_id = self.blobs.put(&record)?;
                self.tier1.append(id, record_id)?;
            }
            None => {
                self.blobs.put(data)?;
            }
        }
        Ok(id)
    }

    /// Materializes the original bytes for `id`. Tier 1 blobs are recomposed
    /// from their parts and re-hashed against `id`.
    pub fn get(&self, id: BlobId) -> Result<Vec<u8>, PrismStoreError> {
        let Some(record_id) = self.tier1.get(id) else {
            return Ok(self.blobs.get(id)?); // Tier 0
        };
        let record = self.blobs.get(record_id)?;
        let (prism, recipe, part_ids) = decode_record(&record)?;
        let parts: Vec<Vec<u8>> = part_ids
            .iter()
            .map(|p| self.blobs.get_unverified(*p))
            .collect::<Result<_, _>>()?;
        let part_refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        let data = self
            .registry
            .recompose(prism, &recipe, &part_refs)
            .ok_or(PrismStoreError::Recompose(id))?;
        if BlobId::of(&data) != id {
            return Err(PrismStoreError::HashMismatch(id));
        }
        Ok(data)
    }

    pub fn contains(&self, id: BlobId) -> bool {
        self.tier1.contains(id) || self.blobs.contains(id)
    }

    /// Whether `id` is stored decomposed (Tier 1) rather than verbatim.
    pub fn is_tier1(&self, id: BlobId) -> bool {
        self.tier1.contains(id)
    }

    /// Durability: parts and records (in the blob store) before the tier1
    /// map that references them, so a crash never leaves a map entry
    /// pointing at content that is not on disk.
    pub fn flush(&mut self) -> Result<(), PrismStoreError> {
        self.blobs.flush()?;
        self.tier1.sync()?;
        Ok(())
    }

    /// The underlying Tier 0 blob store (for size accounting in tests/bench).
    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }
}

/// Record layout: `[prism u16][recipe_len u32][recipe][part_count u32][part ids 32 each]`.
fn encode_record(prism: PrismId, recipe: &[u8], parts: &[BlobId]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 4 + recipe.len() + 4 + parts.len() * 32);
    out.extend_from_slice(&prism.0.to_le_bytes());
    out.extend_from_slice(&(recipe.len() as u32).to_le_bytes());
    out.extend_from_slice(recipe);
    out.extend_from_slice(&(parts.len() as u32).to_le_bytes());
    for p in parts {
        out.extend_from_slice(&p.0);
    }
    out
}

fn decode_record(rec: &[u8]) -> Result<(PrismId, Vec<u8>, Vec<BlobId>), PrismStoreError> {
    let err = || PrismStoreError::Record("truncated");
    let prism = PrismId(u16::from_le_bytes(
        rec.get(..2).ok_or_else(err)?.try_into().unwrap(),
    ));
    let recipe_len =
        u32::from_le_bytes(rec.get(2..6).ok_or_else(err)?.try_into().unwrap()) as usize;
    let recipe = rec.get(6..6 + recipe_len).ok_or_else(err)?.to_vec();
    let mut at = 6 + recipe_len;
    let count =
        u32::from_le_bytes(rec.get(at..at + 4).ok_or_else(err)?.try_into().unwrap()) as usize;
    at += 4;
    let mut parts = Vec::with_capacity(count);
    for _ in 0..count {
        let bytes = rec.get(at..at + 32).ok_or_else(err)?;
        parts.push(BlobId(bytes.try_into().unwrap()));
        at += 32;
    }
    if at != rec.len() {
        return Err(PrismStoreError::Record("trailing bytes"));
    }
    Ok((prism, recipe, parts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_prism_deflate::DeflatePrism;

    /// A real git-style zlib stream (level 1) built with C libz.
    fn zlib(data: &[u8], level: i32) -> Vec<u8> {
        // SAFETY: compress2 writes at most `dl` bytes and updates it.
        unsafe {
            let mut dl = libz_sys::compressBound(data.len() as libz_sys::uLong);
            let mut out = vec![0u8; dl as usize];
            let rc = libz_sys::compress2(
                out.as_mut_ptr(),
                &mut dl,
                data.as_ptr(),
                data.len() as libz_sys::uLong,
                level,
            );
            assert_eq!(rc, libz_sys::Z_OK);
            out.truncate(dl as usize);
            out
        }
    }

    fn deflate_registry() -> Registry {
        let mut r = Registry::new();
        r.register(Box::new(DeflatePrism));
        r
    }

    /// Poorly-compressible bytes so a deflate stream is ~the same size as
    /// its contents (keeps the dedup comparison honest).
    fn noise(len: usize, seed: u32) -> Vec<u8> {
        (0..len as u32)
            .map(|i| (i.wrapping_add(seed).wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect()
    }

    #[test]
    fn tier1_round_trips_a_zlib_stream() {
        let dir = tempfile::tempdir().unwrap();
        let stream = zlib(&noise(200_000, 1), 1);
        let mut s = PrismStore::open(dir.path(), deflate_registry()).unwrap();
        let id = s.put(&stream).unwrap();
        assert!(s.is_tier1(id), "a zlib stream must store decomposed");
        assert_eq!(s.get(id).unwrap(), stream, "Tier 1 recomposes exactly");
    }

    #[test]
    fn non_zlib_falls_back_to_tier0() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"plain bytes, not a compressed stream".to_vec();
        let mut s = PrismStore::open(dir.path(), deflate_registry()).unwrap();
        let id = s.put(&data).unwrap();
        assert!(!s.is_tier1(id), "non-zlib stays Tier 0");
        assert_eq!(s.get(id).unwrap(), data);
    }

    #[test]
    fn tier1_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let stream = zlib(&noise(120_000, 7), 1);
        let id = {
            let mut s = PrismStore::open(dir.path(), deflate_registry()).unwrap();
            let id = s.put(&stream).unwrap();
            s.flush().unwrap();
            id
        };
        let s = PrismStore::open(dir.path(), deflate_registry()).unwrap();
        assert!(s.is_tier1(id), "the tier1 map must persist");
        assert_eq!(s.get(id).unwrap(), stream);
    }

    #[test]
    fn tier1_part_dedup_beats_tier0_for_similar_files() {
        // eight near-identical 1 MiB files, each compressed independently —
        // an asset evolving over commits. The deflate streams are distinct
        // blobs that share nothing (Tier 0 stores eight copies); the inflated
        // bytes differ by one chunk, so Tier 1 dedups to ~one copy + diffs.
        let base = noise(1 << 20, 42);
        let mid = base.len() / 2;
        let streams: Vec<Vec<u8>> = (0..8u8)
            .map(|v| {
                let mut data = base.clone();
                data[mid] ^= v.wrapping_add(1); // a change in the middle
                zlib(&data, 1)
            })
            .collect();

        let stored = |reg: Registry| -> u64 {
            let dir = tempfile::tempdir().unwrap();
            let mut s = PrismStore::open(dir.path(), reg).unwrap();
            for stream in &streams {
                s.put(stream).unwrap();
            }
            s.flush().unwrap();
            s.blobs().counters().bytes_written
        };
        let tier0 = stored(Registry::new()); // empty registry: all verbatim
        let tier1 = stored(deflate_registry());
        eprintln!("tier1 {tier1} vs tier0 {tier0} bytes");
        // Tier 0 stores eight whole copies; Tier 1 stores ~one plus the
        // changed chunks. The win is ~2x here (a mid-file edit cascades CDC
        // boundaries a little); assert a robust majority reduction.
        assert!(
            tier1 * 3 < tier0 * 2,
            "part dedup must cut storage well below Tier 0: tier1 {tier1} vs tier0 {tier0}"
        );
    }
}
