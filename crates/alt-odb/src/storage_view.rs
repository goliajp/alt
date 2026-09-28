//! The physical-layer read surface: how each object is actually stored
//! (tier, prism parts, chunk encodings) and store-wide totals.

use super::*;

impl NativeOdb {
    /// Inspect this blob's physical storage layout for a viewer / audit
    /// tool: tier (0 = verbatim CDC, 1 = prism-decomposed), the prism
    /// id and parts blob list when Tier 1, and a per-leaf-chunk
    /// breakdown showing the chunk store's encoding choice
    /// (Raw / Zstd / Delta-against-base) plus logical vs stored bytes.
    ///
    /// This is the read-side surface for the "what does alt actually
    /// store as the delta" question — alt-web's storage panel calls it
    /// to render the visible answer to `git: 'Binary files differ' /
    /// alt: <here's the physical layout>`.
    pub fn storage_view(&self, oid: &ObjectId) -> Result<Option<StorageView>, OdbError> {
        let Some(entry) = self.map.by_git(oid) else {
            return Ok(None);
        };
        let blob_id = entry.alt;
        let kind = entry.kind;
        let size = entry.size;

        // Tier 1: prism record present in tier1 map.
        let tier1 = if let Some(record_id) = self.tier1.get(blob_id) {
            let record_bytes = self
                .blobs
                .get_unverified(record_id)
                .map_err(OdbError::Store)?;
            match tier1::decode_record(&record_bytes) {
                Ok((prism_id, recipe, parts)) => Some(Tier1Layout {
                    prism: prism_id,
                    recipe_len: recipe.len(),
                    parts,
                    record_blob: record_id,
                }),
                Err(_) => None,
            }
        } else {
            None
        };

        // Chunk view: which blobs to walk depends on the tier.
        //
        // Tier 0 — the original blob lives in the chunk store; walk its
        // leaf chunks directly.
        //
        // Tier 1 — the original blob is *not* in the chunk store. Its
        // bytes are reconstructible only from the prism record + parts.
        // Report all of those: the record's leaf chunks (the recipe's
        // storage cost) plus each part blob's leaf chunks (the
        // member-level dedup surface). This is exactly what makes
        // "stored bytes" meaningful for a Tier 1 blob — the recipe is
        // tiny and parts dedup across files.
        let chunk_layout = match &tier1 {
            None => self.blob_chunk_layout(blob_id)?,
            Some(t1) => self.tier1_chunk_layout(t1)?,
        };

        Ok(Some(StorageView {
            git_oid: *oid,
            blob_id,
            kind,
            logical_size: size,
            tier1,
            chunks: chunk_layout,
        }))
    }

    fn blob_chunk_layout(&self, blob_id: alt_store::BlobId) -> Result<ChunkLayout, OdbError> {
        let leaves = self.blobs.leaf_chunks(blob_id).map_err(OdbError::Store)?;
        let chunks = self.chunk_layouts(&leaves)?;
        let stored_total: u64 = chunks.iter().map(|c| c.stored_len as u64).sum();
        let logical_total: u64 = chunks.iter().map(|c| c.orig_len as u64).sum();
        Ok(ChunkLayout {
            leaf_count: chunks.len(),
            stored_total,
            logical_total,
            chunks,
        })
    }

    /// Aggregate storage facts over every object in this odb. Surfaces
    /// the visible answer to "how much smaller is alt than the
    /// logical content," broken down by tier (verbatim vs prismatic),
    /// by chunk encoding (raw / zstd / delta), and by prism id. One
    /// pass over the object map; pricey but bounded by the repo size.
    pub fn storage_stats(&self) -> Result<StorageStats, OdbError> {
        let mut stats = StorageStats::default();
        // Dedup at the chunk level — if two blobs share a chunk we
        // mustn't count its stored bytes twice. The bookkeeping is
        // per-chunk-id, populated as we see leaves.
        let mut seen_chunks: std::collections::HashSet<alt_store::ChunkId> =
            std::collections::HashSet::new();
        let store = self.blobs.chunk_store();

        // Likewise dedup blobs: two git objects can map to the same
        // alt blob id (rare for unique payloads, possible for empty
        // / repeated objects). We count logical content per git oid
        // (that's the question users actually have), but chunk facts
        // per blob to avoid double-counting parts.
        let mut seen_blobs: std::collections::HashSet<alt_store::BlobId> =
            std::collections::HashSet::new();

        for entry in self.entries() {
            stats.object_count += 1;
            stats.logical_total += entry.size;
            match entry.kind {
                ObjectKind::Blob => stats.blobs += 1,
                ObjectKind::Tree => stats.trees += 1,
                ObjectKind::Commit => stats.commits += 1,
                ObjectKind::Tag => stats.tags += 1,
            }

            let blob_id = entry.alt;
            if !seen_blobs.insert(blob_id) {
                continue;
            }

            let tier1_record_id = self.tier1.get(blob_id);
            if let Some(record_id) = tier1_record_id {
                stats.tier1_count += 1;
                let record_bytes = self
                    .blobs
                    .get_unverified(record_id)
                    .map_err(OdbError::Store)?;
                if let Ok((prism_id, _recipe, parts)) = tier1::decode_record(&record_bytes) {
                    let entry = stats.prisms.entry(prism_id.0).or_default();
                    entry.blobs += 1;
                    entry.parts += parts.len() as u64;
                    // Walk record + part leaves for chunk accounting.
                    self.accumulate_chunks(record_id, &mut seen_chunks, &mut stats, store)?;
                    for p in &parts {
                        if seen_blobs.insert(*p) {
                            self.accumulate_chunks(*p, &mut seen_chunks, &mut stats, store)?;
                        }
                    }
                }
            } else {
                stats.tier0_count += 1;
                self.accumulate_chunks(blob_id, &mut seen_chunks, &mut stats, store)?;
            }
        }
        Ok(stats)
    }

    fn accumulate_chunks(
        &self,
        blob: alt_store::BlobId,
        seen: &mut std::collections::HashSet<alt_store::ChunkId>,
        stats: &mut StorageStats,
        store: &alt_store::ChunkStore,
    ) -> Result<(), OdbError> {
        let leaves = self.blobs.leaf_chunks(blob).map_err(OdbError::Store)?;
        for cid in leaves {
            if !seen.insert(cid) {
                continue;
            }
            let stat = store.stat(cid).map_err(OdbError::Store)?;
            stats.chunks_total += 1;
            stats.stored_total += stat.stored_len as u64;
            stats.chunk_logical_total += stat.orig_len as u64;
            match stat.encoding {
                alt_store::Encoding::Raw => {
                    stats.raw_chunks += 1;
                    stats.raw_stored += stat.stored_len as u64;
                }
                alt_store::Encoding::Zstd => {
                    stats.zstd_chunks += 1;
                    stats.zstd_stored += stat.stored_len as u64;
                }
                alt_store::Encoding::Delta => {
                    stats.delta_chunks += 1;
                    stats.delta_stored += stat.stored_len as u64;
                }
            }
        }
        Ok(())
    }

    fn tier1_chunk_layout(&self, tier1: &Tier1Layout) -> Result<ChunkLayout, OdbError> {
        let mut all = Vec::new();
        all.extend(
            self.blobs
                .leaf_chunks(tier1.record_blob)
                .map_err(OdbError::Store)?,
        );
        for part in &tier1.parts {
            all.extend(self.blobs.leaf_chunks(*part).map_err(OdbError::Store)?);
        }
        let chunks = self.chunk_layouts(&all)?;
        let stored_total: u64 = chunks.iter().map(|c| c.stored_len as u64).sum();
        let logical_total: u64 = chunks.iter().map(|c| c.orig_len as u64).sum();
        Ok(ChunkLayout {
            leaf_count: chunks.len(),
            stored_total,
            logical_total,
            chunks,
        })
    }

    fn chunk_layouts(&self, chunks: &[alt_store::ChunkId]) -> Result<Vec<ChunkInfo>, OdbError> {
        let store = self.blobs.chunk_store();
        let mut out = Vec::with_capacity(chunks.len());
        for &cid in chunks {
            let stat = store.stat(cid).map_err(OdbError::Store)?;
            out.push(ChunkInfo {
                chunk_id: cid,
                encoding: stat.encoding,
                orig_len: stat.orig_len,
                stored_len: stat.stored_len,
            });
        }
        Ok(out)
    }
}

/// What [`NativeOdb::storage_view`] returns: the physical-layer facts
/// behind one git object. Audit-ready: the chunk encoding choice and
/// stored vs logical bytes per leaf are the actual answer to "what
/// does alt store as the delta," and are what the storage panel in
/// alt-web surfaces.
#[derive(Debug, Clone)]
pub struct StorageView {
    pub git_oid: ObjectId,
    pub blob_id: alt_store::BlobId,
    pub kind: alt_git_codec::ObjectKind,
    pub logical_size: u64,
    /// `Some` iff a prism accepted this blob at ingest and the bytes
    /// are stored as parts + recipe. `None` means Tier 0 (verbatim
    /// CDC + zstd).
    pub tier1: Option<Tier1Layout>,
    pub chunks: ChunkLayout,
}

#[derive(Debug, Clone)]
pub struct Tier1Layout {
    pub prism: alt_prism::PrismId,
    pub recipe_len: usize,
    /// Each part is stored as its own blob — the prism layer is what
    /// makes CDC dedup across files actually fire on deflate-wrapped
    /// payloads.
    pub parts: Vec<alt_store::BlobId>,
    /// The blob that holds the prism record itself (recipe + parts
    /// listing). Recoverable from `Tier1Map::get(blob_id)`.
    pub record_blob: alt_store::BlobId,
}

#[derive(Debug, Clone)]
pub struct ChunkLayout {
    pub leaf_count: usize,
    /// Sum of `orig_len` across leaves — should equal `logical_size`
    /// for a Tier 0 blob; for Tier 1 it's the size of the recipe blob
    /// the chunk view is describing, not the original file.
    pub logical_total: u64,
    pub stored_total: u64,
    pub chunks: Vec<ChunkInfo>,
}

#[derive(Debug, Clone)]
pub struct ChunkInfo {
    pub chunk_id: alt_store::ChunkId,
    pub encoding: alt_store::Encoding,
    pub orig_len: u32,
    pub stored_len: u32,
}

/// Aggregate "how does alt physically store this repo" report.
/// All chunk-level counters are deduplicated by chunk id, so a chunk
/// shared by two blobs (the whole point of CDC + prism dedup) is
/// counted once — `stored_total` is the real disk-resident byte budget,
/// not a sum of per-blob views.
#[derive(Debug, Clone, Default)]
pub struct StorageStats {
    pub object_count: u64,
    pub blobs: u64,
    pub trees: u64,
    pub commits: u64,
    pub tags: u64,
    /// Sum of every git object's logical size (the bytes git would have
    /// to keep in its loose / pack form, ignoring git's own
    /// compression). The "X% of logical" headline ratio is
    /// `stored_total / logical_total`.
    pub logical_total: u64,
    pub tier0_count: u64,
    pub tier1_count: u64,
    pub prisms: std::collections::BTreeMap<u16, PrismStats>,
    pub chunks_total: u64,
    /// Sum of `orig_len` over every leaf chunk we walked — useful as a
    /// sanity vs `logical_total` (for Tier 1 they differ: the chunk
    /// view sees recipe + part bytes, not the recomposed original).
    pub chunk_logical_total: u64,
    pub stored_total: u64,
    pub raw_chunks: u64,
    pub raw_stored: u64,
    pub zstd_chunks: u64,
    pub zstd_stored: u64,
    pub delta_chunks: u64,
    pub delta_stored: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PrismStats {
    /// How many blobs this prism accepted at ingest.
    pub blobs: u64,
    /// Total parts produced across those blobs (a docx → 17, etc.).
    pub parts: u64,
}
