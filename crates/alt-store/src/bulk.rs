//! Reading every chunk of a store once, and one chunk without hashing.

use std::collections::HashMap;
use std::rc::Rc;

use crate::index::Location;
use crate::pack::{self, ENC_DELTA, ENC_RAW, ENC_ZSTD, REC_HEADER_LEN};
use crate::{ChunkId, ChunkStore, StoreError, Verify, delta};

impl ChunkStore {
    /// Materializes every chunk exactly once, in delta-chain order, without
    /// re-hashing — the bulk read path for export / full-clone serving. Each
    /// full record is decoded once and its dependent deltas are resolved
    /// against the in-memory base, so a base shared by many deltas is never
    /// decoded twice (unlike per-object [`get`] under a bounded cache, where
    /// reading every object cold re-decodes shared bases repeatedly).
    ///
    /// No per-chunk hash: integrity is the caller's concern at the output
    /// boundary (export → `git fsck` + the round-trip fidelity matrix), a
    /// different trust context than a daily read. Use [`verify_chunk`] /
    /// `alt fsck` to scrub.
    pub(crate) fn each_decoded_here(
        &self,
        mut f: impl FnMut(ChunkId, &[u8]),
    ) -> Result<(), StoreError> {
        // chain forest: full records are roots, each delta hangs off the
        // base it names. Where each record lies is taken from this list
        // and not looked up again
        let index = self.locations()?;
        let mut deps: HashMap<ChunkId, Vec<ChunkId>> = HashMap::with_capacity(index.len());
        let mut roots: Vec<ChunkId> = Vec::new();
        for (&id, &loc) in &index {
            let header = self.read_header(id, loc)?;
            match header.encoding {
                ENC_DELTA => deps
                    .entry(self.read_base_id(id, loc)?)
                    .or_default()
                    .push(id),
                ENC_RAW | ENC_ZSTD => roots.push(id),
                _ => return Err(StoreError::Format("reserved record encoding")),
            }
        }

        // DFS from each root; an Rc carries a decoded base to all its
        // dependents without cloning, freed once the last one is resolved
        let mut emitted = 0usize;
        let mut stack: Vec<(ChunkId, Option<Rc<Vec<u8>>>)> =
            roots.into_iter().map(|id| (id, None)).collect();
        while let Some((id, base)) = stack.pop() {
            let bytes =
                Rc::new(self.decode_no_verify(id, index[&id], base.as_ref().map(|b| &b[..]))?);
            f(id, &bytes);
            emitted += 1;
            for child in deps.remove(&id).unwrap_or_default() {
                stack.push((child, Some(Rc::clone(&bytes))));
            }
        }
        if emitted != index.len() {
            // a delta whose base is absent — only on-disk corruption
            return Err(StoreError::Format("bulk materialize missed chunks"));
        }
        Ok(())
    }

    /// Decodes a chunk's bytes without hashing, reading the stored payload
    /// zero-copy from the sealed mmap (the bulk path's hot read). `loc` is
    /// where its record lies; `base` is `Some` for a delta record, `None`
    /// for a full one.
    fn decode_no_verify(
        &self,
        id: ChunkId,
        loc: Location,
        base: Option<&[u8]>,
    ) -> Result<Vec<u8>, StoreError> {
        let header = self.read_header(id, loc)?;
        // payload: borrowed from the sealed mmap, owned only for the active
        // pack (a handful of records after a compaction)
        let owned;
        let payload: &[u8] = if loc.seq == self.active.seq {
            let mut buf = vec![0u8; header.stored_len as usize];
            pack::read_exact_at(
                &self.active.read,
                &mut buf,
                loc.offset + REC_HEADER_LEN as u64,
            )?;
            owned = buf;
            &owned
        } else {
            let sealed = self.sealed_pack(loc.seq)?;
            let at = loc.offset as usize + REC_HEADER_LEN;
            sealed
                .get(at..at + header.stored_len as usize)
                .ok_or(StoreError::Corrupt {
                    id,
                    reason: "record out of bounds",
                })?
        };
        let data = match (header.encoding, base) {
            (ENC_RAW, None) => payload.to_vec(),
            (ENC_ZSTD, None) => alt_zstd::decode_all(payload).map_err(|_| StoreError::Corrupt {
                id,
                reason: "zstd decode failed",
            })?,
            (ENC_DELTA, Some(base)) => {
                let z = payload.get(32..).ok_or(StoreError::Corrupt {
                    id,
                    reason: "delta payload too short",
                })?;
                delta::decompress_with_base(z, base, header.orig_len as usize).ok_or(
                    StoreError::Corrupt {
                        id,
                        reason: "delta decode failed",
                    },
                )?
            }
            _ => return Err(StoreError::Format("delta/full encoding mismatch")),
        };
        Self::sized(id, data, header.orig_len)
    }

    /// Decodes a single chunk without hashing, zero-copy from the sealed
    /// mmap for the common full-record case (the bulk blob-assembly read).
    /// Multi-chunk blob data and manifest nodes are full records, so they
    /// take the fast path; a delta falls back to the general chain resolve.
    pub(crate) fn decode_unverified_here(&self, id: ChunkId) -> Result<Vec<u8>, StoreError> {
        let loc = self.locate(id)?;
        if self.read_header(id, loc)?.encoding == ENC_DELTA {
            self.resolve(id, Verify::None)
        } else {
            self.decode_no_verify(id, loc, None)
        }
    }
}
