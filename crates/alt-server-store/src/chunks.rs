use crate::requests::{active, check_key, request_row, upload_pin};
use crate::store::{authorize, instance, integer, now, snapshot};
use crate::{ByteStore, Credential, Error, Result, Store};
use alt_remote_protocol::{
    Chunk, ContentEntry, ContentKind, Manifest, Publication, RequestKey, Scope,
};
use std::collections::BTreeMap;
use std::io::Read;

pub use alt_remote_protocol::UploadStatus;
impl Store {
    fn upload_chunks(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<(BTreeMap<String, u64>, Vec<String>)> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(key, &current.scope, &auth.actor_id)?;
        let row = request_row(&mut tx, key)?.ok_or(Error::Unauthorized)?;
        let time = now(&mut tx)?;
        active(&row, time)?;
        let publication: Publication = serde_json::from_value(row.get("publication"))?;
        alt_repository_service::authorize_intent(&publication, &current, &auth, self.limits, time)?;
        let manifest: Manifest = serde_json::from_value(row.get("manifest"))?;
        let chunks: BTreeMap<_, _> = manifest
            .entries
            .iter()
            .flat_map(|e| &e.chunks)
            .map(|c| (c.blake3.clone(), c.bytes))
            .collect();
        let rows = tx.query("SELECT c.blake3 FROM alt_request_content c JOIN alt_blobs b ON b.repository=c.repository AND b.blake3=c.blake3 JOIN alt_pins p ON p.repository=c.repository AND p.blake3=c.blake3 WHERE c.repository=$1 AND c.epoch=$2 AND c.actor_id=$3 AND c.request_id=$4 AND c.verified AND b.state='stored' AND p.pin_id=$5 AND p.expires_at>$6 ORDER BY c.blake3", &[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&upload_pin(key),&integer(time)?])?;
        let mut verified: Vec<_> = rows
            .into_iter()
            .map(|r| r.get::<_, String>(0))
            .filter(|digest| chunks.contains_key(digest))
            .collect();
        verified.sort();
        tx.commit()?;
        Ok((chunks, verified))
    }
    pub fn upload_status(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<UploadStatus> {
        let (_, verified_chunks) = self.upload_chunks(credential, key)?;
        Ok(UploadStatus { verified_chunks })
    }
    pub fn selected_chunk(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<u64> {
        let (chunks, _) = self.upload_chunks(credential, key)?;
        chunks.get(digest).copied().ok_or(Error::Unauthorized)
    }
    pub fn upload_chunk(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
        source: &mut dyn Read,
    ) -> Result<()> {
        let bytes = self.selected_chunk(credential, key, digest)?;
        let proof = self
            .bytes
            .put(&key.scope, digest, bytes, self.limits.chunk_bytes, source)?;
        let entry = ContentEntry {
            kind: ContentKind::Attachment,
            blake3: digest.into(),
            bytes,
            git_oid: None,
            chunks: vec![Chunk {
                offset: 0,
                bytes,
                blake3: digest.into(),
            }],
        };
        self.register_upload(credential, key, &entry, &proof, true)
    }
    /// Assemble exact selected chunks, then pass through the ordinary content
    /// and Git mapping checks. Chunks alone can never publish a ref.
    pub fn assemble_content(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<()> {
        let entries = self.selected_upload(credential, key, digest)?;
        let (_, verified) = self.upload_chunks(credential, key)?;
        let entry = &entries[0];
        if entry
            .chunks
            .iter()
            .any(|c| verified.binary_search(&c.blake3).is_err())
        {
            return Err(Error::Precondition("selected chunks incomplete"));
        }
        let mut reader = ChunkReader {
            bytes: self.bytes.clone(),
            scope: key.scope.clone(),
            chunks: entry.chunks.clone().into_iter(),
            current: None,
        };
        let proof = self.bytes.put(
            &key.scope,
            digest,
            entry.bytes,
            self.limits.object_bytes,
            &mut reader,
        )?;
        for entry in entries {
            self.register_content(credential, key, &entry, &proof)?;
        }
        Ok(())
    }
}
struct ChunkReader {
    bytes: ByteStore,
    scope: Scope,
    chunks: std::vec::IntoIter<Chunk>,
    current: Option<std::fs::File>,
}
impl Read for ChunkReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(file) = &mut self.current {
                let count = file.read(output)?;
                if count > 0 {
                    return Ok(count);
                }
                self.current = None;
            }
            let Some(chunk) = self.chunks.next() else {
                return Ok(0);
            };
            self.current = Some(
                self.bytes
                    .read(&self.scope, &chunk.blake3, chunk.bytes)
                    .map_err(std::io::Error::other)?,
            );
        }
    }
}
