use super::*;

impl ByteStore {
    /// Read a proof member without allocating another descriptor. The proof's
    /// file identity is rechecked before granting the bounded reader.
    pub fn read_durable_segment(
        &self,
        proof: &DurableSegment,
        digest: &[u8; 32],
    ) -> Result<StoredReader> {
        self.check_segment_identity(proof)?;
        let member = proof
            .index
            .find(digest)
            .ok_or(Error::Integrity("missing proof member"))?;
        Ok(StoredReader {
            file: proof.file.clone(),
            start: member.offset(),
            bytes: member.bytes(),
            position: 0,
        })
    }

    /// Check the current file and exact database-selected index/member range.
    /// This does not claim payload integrity: callers must verify canonical
    /// identities through their existing publication/read validation path.
    pub fn read_segment(
        &self,
        scope: &Scope,
        digest: &str,
        bytes: u64,
        location: &SegmentLocation,
    ) -> Result<StoredReader> {
        check_digest(digest)?;
        let digest = alt_hash::blake3::Hash::from_hex(digest)
            .map_err(|_| Error::Integrity("segment digest"))?;
        let path = self.segment_path(scope, &location.segment_id)?;
        let mut file = open_segment(&path)?;
        let index = Segment::open(&mut file, Limits::default())
            .map_err(segment_error)?
            .index()
            .clone();
        let member = index
            .find(digest.as_bytes())
            .ok_or(Error::Integrity("missing segment member"))?;
        if index.digest() != &location.index_digest
            || member.offset() != location.offset
            || member.bytes() != bytes
        {
            return Err(Error::Integrity("segment location differs"));
        }
        Ok(StoredReader {
            file: Arc::new(file),
            start: member.offset(),
            bytes,
            position: 0,
        })
    }

    pub fn verify_segment_member(
        &self,
        scope: &Scope,
        digest: &str,
        bytes: u64,
        location: &SegmentLocation,
    ) -> Result<StoredReader> {
        let mut reader = self.read_segment(scope, digest, bytes, location)?;
        let mut hash = alt_hash::blake3::Hasher::new();
        let mut buffer = [0; 65536];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if hash.finalize().to_hex().as_str() != digest {
            return Err(Error::Integrity("segment member hash"));
        }
        reader.rewind()?;
        Ok(reader)
    }
}
