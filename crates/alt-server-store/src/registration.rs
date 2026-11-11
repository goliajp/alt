//! Bounded upload registration within one current-authority transaction. Byte
//! persistence/identity checks remain separate and every proof is rechecked
//! under the same repository barrier as the scalar registration path.
use crate::Jsonb;
use std::collections::BTreeMap;

use alt_remote_protocol::{ContentEntry, Publication, RequestKey};
use alt_repository_service::authorize_intent;

use crate::requests::{active, check_key, git_kind, upload_request_row};
use crate::store::{authorize, instance, integer, now, snapshot, unsigned};
use crate::{Credential, DurableBytes, Error, Result, Store};

/// Maximum logical entries in one registration transaction. Callers creating
/// separate loose files must also honor `MAX_PUT_INPUTS` for open file proofs;
/// segment members share their backing file and existing members reopen on use.
pub const MAX_REGISTER_CONTENTS: usize = alt_byte_segment::MAX_MEMBERS;

impl Store {
    /// Register selected whole contents atomically. An error rolls back this
    /// batch; earlier batches remain ordinary resumable upload progress. This
    /// does not publish refs, extend the request, or weaken durable-byte proof.
    pub fn register_contents(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        entries: &[(&ContentEntry, &DurableBytes)],
    ) -> Result<()> {
        if entries.is_empty() || entries.len() > MAX_REGISTER_CONTENTS {
            return Err(Error::Limit("content registration batch"));
        }
        for &(entry, proof) in entries {
            self.validate_upload(key, entry, proof)?;
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(key, &current.scope, &auth.actor_id)?;
        let request = upload_request_row(&mut tx, key)?
            .ok_or(Error::Precondition("request not registered"))?;
        let time = now(&mut tx)?;
        active(&request, time)?;
        let publication: Publication = request.get::<_, Jsonb>("publication").read()?;
        authorize_intent(&publication, &current, &auth, self.limits, time)?;
        let repository = &key.scope.repository_instance_id;
        let epoch = integer(key.scope.recovery_epoch)?;
        let unique: BTreeMap<_, _> = entries
            .iter()
            .map(|(entry, _)| (entry.blake3.as_str(), *entry))
            .collect();
        let digests: Vec<_> = unique.keys().copied().collect();
        let selections: BTreeMap<String, (Vec<ContentEntry>, bool)> = tx.query(
            "SELECT blake3,selected_entries,verified FROM alt_request_content WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 AND blake3=ANY($5)",
            &[repository, &epoch, &key.actor_id, &key.request_id, &digests],
        )?.into_iter().map(|row| Ok((row.get(0), (row.get::<_, Jsonb>(1).read()?, row.get(2)))))
            .collect::<Result<_>>()?;
        let blobs: BTreeMap<String, (String, bool, u64)> = tx.query(
            "SELECT blake3,state,accounted,bytes FROM alt_blobs WHERE repository=$1 AND blake3=ANY($2)",
            &[repository, &digests],
        )?.into_iter().map(|row| Ok((row.get(0), (row.get(1), row.get(2), unsigned(row.get(3))?))))
            .collect::<Result<_>>()?;
        let mut mappings = BTreeMap::new();
        for &(entry, proof) in entries {
            let (selected, _) = selections
                .get(&entry.blake3)
                .ok_or(Error::Precondition("request content selection missing"))?;
            if !selected.contains(entry) {
                return Err(Error::Integrity("upload scope or selection"));
            }
            let (state, _, bytes) = blobs
                .get(&entry.blake3)
                .ok_or(Error::Precondition("upload pin expired"))?;
            if state == "deleting" || *bytes != entry.bytes {
                return Err(Error::Precondition("upload content fenced by GC"));
            }
            self.bytes.check_proof(proof)?;
            if let Some(oid) = &entry.git_oid {
                let mapping = (git_kind(entry.kind)?, entry.blake3.as_str());
                if let Some(previous) = mappings.insert(oid.as_str(), mapping)
                    && previous != mapping
                {
                    return Err(Error::Integrity("Git identity collision"));
                }
            }
        }
        let oids: Vec<_> = mappings.keys().copied().collect();
        let existing: BTreeMap<String, (String, String)> = if oids.is_empty() {
            BTreeMap::new()
        } else {
            tx.query(
                "SELECT oid,kind,blake3 FROM alt_objects WHERE repository=$1 AND oid=ANY($2)",
                &[repository, &oids],
            )?
            .into_iter()
            .map(|row| (row.get(0), (row.get(1), row.get(2))))
            .collect()
        };
        let mut missing = Vec::new();
        let mut kinds = Vec::new();
        let mut payloads = Vec::new();
        for (oid, (kind, digest)) in &mappings {
            if let Some((old_kind, old_digest)) = existing.get(*oid) {
                if old_kind.as_str() != *kind || old_digest.as_str() != *digest {
                    return Err(Error::Integrity("Git identity collision"));
                }
            } else {
                missing.push(*oid);
                kinds.push(*kind);
                payloads.push(*digest);
            }
        }
        let mut stored = 0u64;
        let mut account = false;
        let mut accepted = 0u64;
        let mut settle = false;
        for (digest, entry) in &unique {
            if !blobs[*digest].1 {
                account = true;
                stored = stored
                    .checked_add(entry.bytes)
                    .ok_or(Error::Limit("registered bytes"))?;
            }
            let (selected, verified) = &selections[*digest];
            if !verified && !selected.is_empty() {
                settle = true;
                accepted = accepted
                    .checked_add(entry.bytes)
                    .ok_or(Error::Limit("accepted bytes"))?;
            }
        }
        if account {
            let release = unsigned(request.get("reserved_bytes"))?.min(stored);
            if tx.execute("UPDATE alt_repositories SET stored_bytes=stored_bytes+$2,reserved_bytes=reserved_bytes-$3 WHERE id=$1 AND $2::bigint-$3::bigint<=quota_bytes-stored_bytes-reserved_bytes-link_bytes", &[repository, &integer(stored)?, &integer(release)?])? != 1 {
                return Err(Error::Limit("repository quota"));
            }
            tx.execute("UPDATE alt_requests SET reserved_bytes=reserved_bytes-$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[repository, &epoch, &key.actor_id, &key.request_id, &integer(release)?])?;
        }
        crate::segments::register_locations(&mut tx, key, entries, &self.bytes)?;
        if tx.execute("UPDATE alt_blobs SET state='stored',accounted=true WHERE repository=$1 AND blake3=ANY($2)", &[repository, &digests])? != unique.len() as u64 {
            return Err(Error::Precondition("upload pin expired"));
        }
        if !missing.is_empty() {
            tx.execute("INSERT INTO alt_objects(repository,oid,kind,blake3) SELECT $1,oid,kind,blake3 FROM unnest($2::text[],$3::text[],$4::text[]) AS objects(oid,kind,blake3)", &[repository, &missing, &kinds, &payloads])?;
        }
        if settle {
            crate::cloud_quota::accept(&mut tx, &self.spec, key, accepted)?;
        }
        if tx.execute("UPDATE alt_request_content SET verified=true WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 AND blake3=ANY($5)", &[repository, &epoch, &key.actor_id, &key.request_id, &digests])? != unique.len() as u64 {
            return Err(Error::Precondition("request content selection missing"));
        }
        // Clock-sensitive credentials, request TTL and Cloud service authority
        // must still hold after all checks and writes in this bounded batch.
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let time = now(&mut tx)?;
        active(&request, time)?;
        authorize_intent(&publication, &current, &auth, self.limits, time)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
}
