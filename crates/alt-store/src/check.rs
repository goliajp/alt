//! The full check of a store: every record of every file is read and
//! verified, whether or not anything ever looked it up.
//!
//! Ordinary reads verify what they return and nothing else, and the index
//! runs are trusted from their checksums alone. A record that no read has
//! touched can therefore be damaged without any command noticing; these
//! functions are what notices.

use alt_os::map::Mmap;

use crate::index::Location;
use crate::pack::{self, ENC_DELTA, ENC_RAW, ENC_ZSTD, REC_HEADER_LEN, RecordHeader};
use crate::{BlobStore, ChunkId, ChunkStore, StoreError, Verify, delta};

/// How many records a full check read and verified.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StoreChecked {
    /// records in the packs, superseded ones included
    pub chunk_records: u64,
    /// records in the blob map, one per blob of more than one chunk
    pub blob_records: u64,
}

impl ChunkStore {
    /// Deep scrub: resolve `id` re-hashing every layer from disk (bypassing
    /// the read cache), so corruption is localized to the failing layer
    /// rather than only detected at the boundary.
    pub fn verify_chunk(&self, id: ChunkId) -> Result<(), StoreError> {
        self.resolve(id, Verify::Deep).map(drop)
    }

    /// Resolves a chunk re-hashing every layer; the bytes are returned so the
    /// blob deep-verify can reassemble and check the blob boundary too.
    pub(crate) fn read_deep(&self, id: ChunkId) -> Result<Vec<u8>, StoreError> {
        self.resolve(id, Verify::Deep)
    }

    /// Walks every pack from its first byte to its last: each record must
    /// decode to bytes that hash to its id, the packs must hold nothing but
    /// records, and the chunk index must name exactly these records.
    /// Returns how many records there are.
    pub fn check(&self) -> Result<u64, StoreError> {
        self.pinned()?.check_here()
    }

    fn check_here(&self) -> Result<u64, StoreError> {
        self.index.verify_runs()?;
        let mut records = 0u64;
        let mut indexed = 0usize;
        let seqs = self.sealed.keys().copied().chain([self.active.seq]);
        for seq in seqs {
            let active;
            let data: &[u8] = if seq == self.active.seq {
                // SAFETY: packs are append-only, so the mapped range never
                // changes under the map
                active = unsafe { Mmap::map(&self.active.read)? };
                active
                    .get(..self.active.len as usize)
                    .ok_or(StoreError::Format("active pack shorter than its records"))?
            } else {
                self.sealed_pack(seq)?
            };
            pack::check_file_header(data)?;
            let (found, valid_len) = pack::scan_from(data, pack::HEADER_LEN);
            if valid_len != data.len() as u64 {
                return Err(StoreError::Format("pack holds bytes that are not records"));
            }
            for (header, offset) in found {
                let at = offset as usize + REC_HEADER_LEN;
                self.check_record(&header, &data[at..at + header.stored_len as usize])?;
                if self
                    .index
                    .check_record(header.id, Location { seq, offset })?
                {
                    indexed += 1;
                }
                records += 1;
            }
        }
        if indexed != self.index.run_entries() {
            return Err(StoreError::Format(
                "chunk index names a record no pack holds",
            ));
        }
        Ok(records)
    }

    /// Decodes one record as it sits in its pack and hashes the result.
    pub(crate) fn check_record(
        &self,
        header: &RecordHeader,
        payload: &[u8],
    ) -> Result<(), StoreError> {
        let id = header.id;
        let corrupt = |reason| StoreError::Corrupt { id, reason };
        let data = match header.encoding {
            ENC_RAW => payload.to_vec(),
            ENC_ZSTD => alt_zstd::decode_all(payload).map_err(|_| corrupt("zstd decode failed"))?,
            ENC_DELTA => {
                let base = payload
                    .get(..32)
                    .ok_or(corrupt("delta payload too short"))?;
                // the base comes back verified against its own id
                let base = self.get(ChunkId(base.try_into().unwrap()))?;
                delta::decompress_with_base(&payload[32..], &base, header.orig_len as usize)
                    .ok_or(corrupt("delta decode failed"))?
            }
            _ => return Err(StoreError::Format("reserved record encoding")),
        };
        Self::check_hash(id, &Self::sized(id, data, header.orig_len)?)
    }
}

impl BlobStore {
    /// Full check of the chunk store and of the blob map over it: besides
    /// [`ChunkStore::check`], every blob the map records is assembled from
    /// its manifest and hashed against its id.
    pub fn check(&self) -> Result<StoreChecked, StoreError> {
        self.check_upto(u64::MAX)
    }

    /// [`Self::check`] without assembling the blobs whose records begin
    /// at or past byte `upto` of the blob map.
    pub fn check_upto(&self, upto: u64) -> Result<StoreChecked, StoreError> {
        let chunk_records = self.chunk_store().check()?;
        let blob_records = self.map()?.check(upto, |blob, total_len| {
            if self.get(blob)?.len() as u64 != total_len {
                return Err(StoreError::Format("blobmap record corrupt"));
            }
            Ok(())
        })?;
        Ok(StoreChecked {
            chunk_records,
            blob_records,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlobId, BlobOptions};
    use std::os::unix::fs::FileExt;

    const BLOBS: usize = 600;

    fn body(n: usize) -> Vec<u8> {
        // each blob spans several chunks, so each gets a blob map record
        let mut out = Vec::new();
        let mut word = (n as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        while out.len() < 6000 {
            word ^= word << 13;
            word ^= word >> 7;
            word ^= word << 17;
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }

    fn options() -> BlobOptions {
        BlobOptions {
            cdc: alt_cdc::Params {
                min: 512,
                avg: 1024,
                max: 2048,
            },
            ..BlobOptions::default()
        }
    }

    /// A store whose records are all behind the index runs, and its blobs.
    fn filled(dir: &std::path::Path) -> Vec<BlobId> {
        let mut store = BlobStore::open_with(dir, options()).unwrap();
        let ids: Vec<BlobId> = (0..BLOBS).map(|n| store.put(&body(n)).unwrap()).collect();
        store.flush().unwrap();
        store.index_tail().unwrap();
        ids
    }

    fn flip(path: &std::path::Path, at: u64) {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let mut byte = [0u8; 1];
        file.read_exact_at(&mut byte, at).unwrap();
        byte[0] ^= 0x40;
        file.write_all_at(&byte, at).unwrap();
    }

    #[test]
    fn a_sound_store_passes_and_every_record_is_counted() {
        let tmp = alt_os::temp::tempdir().unwrap();
        filled(tmp.path());
        let store = BlobStore::open_with(tmp.path(), options()).unwrap();
        let checked = store.check().unwrap();
        assert_eq!(checked.blob_records, BLOBS as u64);
        assert!(checked.chunk_records > 2 * BLOBS as u64, "{checked:?}");
    }

    #[test]
    fn damage_in_a_blobmap_record_nothing_reads_is_found() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let ids = filled(tmp.path());
        // record 300 of 600, 80 bytes each after a 5-byte header: behind
        // the index runs, so opening the store does not read it
        flip(&tmp.path().join("manifests/blobmap"), 5 + 300 * 80 + 40);
        let store = BlobStore::open_with(tmp.path(), options()).unwrap();
        assert_eq!(store.get(ids[0]).unwrap(), body(0));
        assert_eq!(store.get(ids[599]).unwrap(), body(599));
        let err = store.check().unwrap_err();
        assert!(
            matches!(err, StoreError::Format("blobmap record corrupt")),
            "{err:?}"
        );
    }

    #[test]
    fn damage_in_a_chunk_nothing_reads_is_found() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let ids = filled(tmp.path());
        let pack = tmp.path().join("pack-00000001.altpack");
        let len = std::fs::metadata(&pack).unwrap().len();
        // the last byte of the pack is payload of the last chunk written
        flip(&pack, len - 1);
        let store = BlobStore::open_with(tmp.path(), options()).unwrap();
        assert_eq!(store.get(ids[0]).unwrap(), body(0));
        let err = store.check().unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "{err:?}");
    }

    #[test]
    fn a_damaged_or_incomplete_index_run_is_found() {
        let tmp = alt_os::temp::tempdir().unwrap();
        filled(tmp.path());
        let dir = tmp.path().join("chunks.idx");
        let run = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .max_by_key(|path| std::fs::metadata(path).unwrap().len())
            .unwrap();
        // an entry's position, in the middle of the file
        flip(&run, std::fs::metadata(&run).unwrap().len() / 2);
        let store = BlobStore::open_with(tmp.path(), options()).unwrap();
        let err = store.check().unwrap_err();
        assert!(
            matches!(err, StoreError::Format("index run corrupt")),
            "{err:?}"
        );
    }
}
