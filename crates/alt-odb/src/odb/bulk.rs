//! Reading every object of the database in one pass, and changing how
//! objects are stored.

use alt_git_codec::{ObjectId, RawObject};
use alt_store::{BlobId, CompactReport, StoreError};

use super::Odb;
use crate::OdbError;
use crate::map::MapEntry;

impl Odb {
    /// [`Self::get`] with no hashing of what is read: for a caller that
    /// had every record of the store verified before, as `alt fsck` has
    /// when it walks what the refs lead to. A Tier 1 blob is recomposed
    /// and hashed all the same.
    pub fn get_unverified(&self, oid: &ObjectId) -> Result<Option<RawObject>, OdbError> {
        let Some(entry) = self.map.by_git(oid)? else {
            return Ok(None);
        };
        let data = match self.blobs.get_unverified(entry.alt) {
            Err(StoreError::BlobNotFound(_)) => self.fetch_blob(entry.alt)?,
            read => read?,
        };
        if data.len() as u64 != entry.size {
            return Err(OdbError::SizeMismatch(*oid, entry.size));
        }
        let kind = entry.kind;
        Ok(Some(RawObject { kind, data }))
    }

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

impl Odb {
    /// Compacts the underlying chunk store, reclaiming the dead weight left
    /// by lineage delta re-encoding. Object identities and the map are
    /// untouched — only physical storage is rewritten.
    pub fn compact(&mut self) -> Result<CompactReport, OdbError> {
        self.acquire()?;
        Ok(self.blobs.compact()?)
    }
}

impl Odb {
    /// Re-encodes `old`'s content as a lineage delta against `new`'s
    /// (same-path predecessor → successor). Identity is untouched; only
    /// the storage form changes. Returns whether a re-encoding happened.
    pub fn lineage_delta(&mut self, old: &ObjectId, new: &ObjectId) -> Result<bool, OdbError> {
        self.acquire()?;
        let (Some(old_entry), Some(new_entry)) = (self.map.by_git(old)?, self.map.by_git(new)?)
        else {
            return Ok(false);
        };
        let (old_alt, new_alt) = (old_entry.alt, new_entry.alt);
        Ok(self.blobs.lineage_delta(old_alt, new_alt)?)
    }
}
