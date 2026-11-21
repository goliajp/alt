//! Reading every object of the database in one pass.

use alt_store::BlobId;

use super::Odb;
use crate::OdbError;
use crate::map::MapEntry;

impl Odb {
    /// Bulk-materializes every mapped object's bytes exactly once, without
    /// per-object re-hashing — the read path for export / full-clone serving.
    /// Single-chunk objects (the lineage-delta'd ones) go through the chunk
    /// store's decode-once forest, so a base shared by many versions is
    /// decoded just once; multi-chunk blobs (not delta'd) are assembled
    /// normally; Tier 1 (prism-decomposed) blobs come back through
    /// `fetch_blob`, which already recomposes through the registry and
    /// re-hashes the result against the recorded blob id (the prism
    /// integrity boundary — without it a corrupt part could yield wrong
    /// bytes silently). Tier 0 integrity still belongs to the output
    /// boundary (export → `git fsck`), not to each read.
    pub fn for_each_object_unverified(
        &self,
        mut f: impl FnMut(&MapEntry, &[u8]),
    ) -> Result<(), OdbError> {
        let entries = self.map.entries()?;
        let mut by_alt: std::collections::HashMap<BlobId, Vec<u32>> =
            std::collections::HashMap::with_capacity(entries.len());
        for (at, entry) in entries.iter().enumerate() {
            by_alt.entry(entry.alt).or_default().push(at as u32);
        }
        self.blobs
            .chunk_store()
            .for_each_decoded(|chunk_id, bytes| {
                // Skip chunks that aren't a top-level git object: this includes
                // prism parts and recipe-record blobs, which are stored
                // alongside but have no MapEntry of their own.
                for &at in by_alt.get(&BlobId(chunk_id.0)).into_iter().flatten() {
                    f(&entries[at as usize], bytes);
                }
            })?;
        for entry in &entries {
            if self.tier1()?.contains(entry.alt)? {
                // Tier 1: recompose through the registry; fetch_blob does the
                // recompose+rehash so we never emit silently-wrong bytes.
                let bytes = self.fetch_blob(entry.alt)?;
                f(entry, &bytes);
            } else if self.blobs.is_multi_chunk(entry.alt)? {
                let bytes = self.blobs.get_unverified(entry.alt)?;
                f(entry, &bytes);
            }
        }
        Ok(())
    }
}
