use crate::Jsonb;
use crate::database::Transaction;
use crate::store::{authorize, instance, integer, now, snapshot, unsigned};
use crate::{Credential, DurableBytes, Error, Result, Store};
use alt_remote_protocol::{
    ContentEntry, ContentKind, Manifest, Publication, RequestKey, RequestObservation, RequestState,
};
use alt_repository_service::authorize_intent;
use std::collections::BTreeMap;

impl Store {
    /// Register an immutable intent and reserve its bounded upload space.
    /// Existing request identities are compared before any new reservation.
    pub fn begin(
        &mut self,
        credential: &Credential,
        publication: &Publication,
        manifest: &Manifest,
        ttl_ms: u64,
    ) -> Result<RequestState> {
        publication.validate(self.limits)?;
        crate::telemetry::request(&publication.key);
        manifest.validate_publication(publication, self.limits)?;
        if ttl_ms == 0 || ttl_ms > 86_400_000 {
            return Err(Error::Limit("upload lifetime"));
        }
        // Git objects the segments hold need no upload. Segments only gain objects, so
        // a set opened before the transaction grants nothing a later one would not.
        let segments = self.published_segments(&publication.key.scope.repository_instance_id)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let snapshot = snapshot(
            &mut tx,
            &self.spec,
            &publication.key.scope.repository_instance_id,
        )?;
        let auth = authorize(&mut tx, &self.spec, credential, &snapshot)?;
        let time = now(&mut tx)?;
        let digest = authorize_intent(publication, &snapshot, &auth, self.limits, time)?;
        if let Some(row) = request_row(&mut tx, &publication.key)? {
            let previous: String = row.get("digest");
            if previous != digest {
                return Err(Error::Precondition(
                    "request ID reused with different parameters",
                ));
            }
            return Ok(row.get::<_, Jsonb>("result").read()?);
        }
        let expires = time
            .checked_add(ttl_ms)
            .ok_or(Error::Limit("upload lifetime"))?;
        let contents = selected_contents(manifest, &segments)?;
        let repository = &publication.key.scope.repository_instance_id;
        let mut reservation = 0u64;
        let selected: Vec<_> = contents.iter().collect();
        for batch in selected.chunks(crate::database::BATCH_ROWS) {
            let digests: Vec<_> = batch.iter().map(|(digest, _)| digest.as_str()).collect();
            let existing: BTreeMap<String, (i64, String)> = tx
                .query("SELECT blake3,bytes,state FROM alt_blobs WHERE repository=$1 AND blake3=ANY($2)", &[repository, &digests])?
                .into_iter().map(|row| (row.get(0), (row.get(1), row.get(2)))).collect();
            let mut missing = Vec::new();
            let mut sizes = Vec::new();
            for (digest, content) in batch {
                let stored = match existing.get(digest.as_str()) {
                    Some((bytes, state)) => {
                        if unsigned(*bytes)? != content.bytes {
                            return Err(Error::Integrity("content size collision"));
                        }
                        if state == "deleting" {
                            return Err(Error::Precondition(
                                "content GC in progress; retry same intent",
                            ));
                        }
                        state == "stored"
                    }
                    None => {
                        missing.push(digest.as_str());
                        sizes.push(integer(content.bytes)?);
                        false
                    }
                };
                if !stored && !content.settled() {
                    reservation = reservation
                        .checked_add(content.bytes)
                        .ok_or(Error::Limit("upload reservation"))?;
                }
            }
            if !missing.is_empty() {
                tx.execute("INSERT INTO alt_blobs(repository,blake3,bytes,state,created_at) SELECT $1,digest,bytes,'staging',$4 FROM unnest($2::text[],$3::bigint[]) AS content(digest,bytes)", &[repository,&missing,&sizes,&integer(time)?])?;
            }
        }
        if tx.execute("UPDATE alt_repositories SET reserved_bytes=reserved_bytes+$2 WHERE id=$1 AND $2<=quota_bytes-stored_bytes-reserved_bytes-link_bytes",&[repository,&integer(reservation)?])?!=1 { return Err(Error::Limit("repository quota")); }
        let state = RequestState::Receiving {
            digest: digest.clone(),
            expires_at_unix_ms: expires,
        };
        tx.execute("INSERT INTO alt_requests(repository,epoch,actor_id,request_id,digest,publication,manifest,state,result,expires_at,reserved_bytes) VALUES($1,$2,$3,$4,$5,$6,$7,'receiving',$8,$9,$10)",&[repository,&integer(publication.key.scope.recovery_epoch)?,&publication.key.actor_id,&publication.key.request_id,&digest,&Jsonb::from(&publication)?,&Jsonb::from(&manifest)?,&Jsonb::from(&state)?,&integer(expires)?,&integer(reservation)?])?;
        let mut ingress_reservation = 0u64;
        let pin_id = upload_pin(&publication.key);
        for batch in selected.chunks(crate::database::BATCH_ROWS) {
            let digests: Vec<_> = batch.iter().map(|(digest, _)| digest.as_str()).collect();
            let published: std::collections::BTreeSet<String> = tx.query("SELECT b.blake3 FROM alt_blobs b WHERE b.repository=$1 AND b.blake3=ANY($2) AND b.state='stored' AND EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=b.repository AND p.blake3=b.blake3 AND p.expires_at IS NULL)", &[repository,&digests])?
                .into_iter().map(|row| row.get(0)).collect();
            let mut verified = Vec::with_capacity(batch.len());
            let mut entries = Vec::with_capacity(batch.len());
            let mut chunk_sizes = Vec::with_capacity(batch.len());
            for (digest, content) in batch {
                let present = published.contains(digest.as_str()) || content.settled();
                if !present && !content.entries.is_empty() {
                    ingress_reservation = ingress_reservation
                        .checked_add(content.bytes)
                        .ok_or(Error::Limit("Cloud ingress reservation"))?;
                }
                verified.push(present);
                entries.push(Jsonb::from(&content.entries)?);
                chunk_sizes.push(content.chunk.then(|| integer(content.bytes)).transpose()?);
            }
            tx.execute("INSERT INTO alt_request_content(repository,epoch,actor_id,request_id,blake3,verified,selected_entries,chunk_bytes) SELECT $1,$2,$3,$4,digest,verified,entries,bytes FROM unnest($5::text[],$6::boolean[],$7::jsonb[],$8::bigint[]) AS content(digest,verified,entries,bytes)", &[repository,&integer(publication.key.scope.recovery_epoch)?,&publication.key.actor_id,&publication.key.request_id,&digests,&verified,&entries,&chunk_sizes])?;
            pin_many(&mut tx, repository, &pin_id, &digests, Some(expires))?;
        }
        crate::cloud_quota::reserve(&mut tx, &self.spec, &publication.key, ingress_reservation)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(state)
    }
    pub fn query(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<RequestObservation> {
        key.validate()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if key.scope.installation_id != self.spec.installation_id || auth.actor_id != key.actor_id {
            return Err(Error::Unauthorized);
        }
        // Old epochs may return retained facts. Absence is meaningful only in
        // the current epoch; never turn a restored snapshot's gap into replay.
        let row = request_row(&mut tx, key)?;
        if let Some(row) = &row {
            check_request_attribution(row, &auth)?;
        }
        let result = match row {
            Some(row)
                if key.scope.recovery_epoch != current.scope.recovery_epoch
                    && matches!(
                        row.get::<_, String>("state").as_str(),
                        "receiving" | "ready"
                    ) =>
            {
                RequestObservation::Unknown { key: key.clone() }
            }
            Some(row) => RequestObservation::Recorded {
                key: key.clone(),
                result: Box::new(row.get::<_, Jsonb>("result").read()?),
            },
            None if key.scope.recovery_epoch == current.scope.recovery_epoch => {
                RequestObservation::NotSeen { key: key.clone() }
            }
            None => RequestObservation::Unknown { key: key.clone() },
        };
        tx.commit()?;
        crate::telemetry::request(key);
        if let RequestObservation::Recorded { result, .. } = &result
            && let RequestState::Committed { receipt } = result.as_ref()
        {
            // A retained receipt identifies the original transaction, without
            // counting this read/replay as another publication commit.
            crate::telemetry::committed(receipt);
        }
        Ok(result)
    }
    /// Store proof must refer to an entry explicitly selected in this intent.
    /// No object becomes readable through refs merely by uploading bytes.
    pub fn register_content(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        entry: &ContentEntry,
        proof: &DurableBytes,
    ) -> Result<()> {
        self.register_upload(credential, key, entry, proof, false)
    }
    pub(crate) fn validate_upload(
        &self,
        key: &RequestKey,
        entry: &ContentEntry,
        proof: &DurableBytes,
    ) -> Result<()> {
        if proof.scope() != &key.scope
            || proof.blake3() != entry.blake3
            || proof.bytes() != entry.bytes
        {
            return Err(Error::Integrity("upload scope or selection"));
        }
        Manifest {
            protocol_version: alt_remote_protocol::VERSION,
            object_format: alt_remote_protocol::ObjectFormat::Sha1,
            entries: vec![entry.clone()],
        }
        .validate(self.limits)?;
        crate::ByteStore::verify_chunk_reader(self.bytes.read_proof(proof)?, entry)?;
        // Verify the Git mapping before inserting it, so a malicious upload
        // cannot poison a future legitimate OID with unrelated payload bytes.
        if let Some(oid) = &entry.git_oid {
            let oid = oid.parse().map_err(|_| Error::Integrity("Git object id"))?;
            let kind = git_kind(entry.kind)?
                .parse()
                .map_err(|_| Error::Integrity("Git object kind"))?;
            let reader = self.bytes.read_proof(proof)?;
            alt_repository_service::verify_object(
                oid,
                alt_repository_service::ObjectReader {
                    kind,
                    bytes: proof.bytes(),
                    reader: Box::new(reader),
                },
                self.limits.into(),
            )?;
        }
        Ok(())
    }
    pub(crate) fn register_upload(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        entry: &ContentEntry,
        proof: &DurableBytes,
        chunk: bool,
    ) -> Result<()> {
        if proof.location().is_some() || proof.existing() {
            return Err(Error::Precondition(
                "segment or reused content requires batch registration",
            ));
        }
        self.validate_upload(key, entry, proof)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(key, &current.scope, &auth.actor_id)?;
        let row = upload_request_row(&mut tx, key)?
            .ok_or(Error::Precondition("request not registered"))?;
        let time = now(&mut tx)?;
        active(&row, time)?;
        let publication: Publication = row.get::<_, Jsonb>("publication").read()?;
        authorize_intent(&publication, &current, &auth, self.limits, time)?;
        let selection = upload_selection(&mut tx, key, &entry.blake3)?
            .ok_or(Error::Precondition("request content selection missing"))?;
        let selected = if chunk {
            entry.kind == ContentKind::Attachment
                && entry.git_oid.is_none()
                && selection.chunk_bytes == Some(entry.bytes)
        } else {
            selection.entries.contains(entry)
        };
        if !selected
            || proof.scope() != &key.scope
            || proof.blake3() != entry.blake3
            || proof.bytes() != entry.bytes
        {
            return Err(Error::Integrity("upload scope or selection"));
        }
        let repository = &key.scope.repository_instance_id;
        let blob = tx
            .query_opt(
                "SELECT state,accounted,bytes FROM alt_blobs WHERE repository=$1 AND blake3=$2",
                &[repository, &entry.blake3],
            )?
            .ok_or(Error::Precondition("upload pin expired"))?;
        if blob.get::<_, String>(0) == "deleting" || unsigned(blob.get(2))? != entry.bytes {
            return Err(Error::Precondition("upload content fenced by GC"));
        }
        self.bytes.check_proof(proof)?;
        if !blob.get::<_, bool>(1) {
            let reservation = unsigned(row.get("reserved_bytes"))?;
            let release = reservation.min(entry.bytes);
            if tx.execute("UPDATE alt_repositories SET stored_bytes=stored_bytes+$2,reserved_bytes=reserved_bytes-$3 WHERE id=$1 AND $2::bigint-$3::bigint<=quota_bytes-stored_bytes-reserved_bytes-link_bytes",&[repository,&integer(entry.bytes)?,&integer(release)?])?!=1 { return Err(Error::Limit("repository quota")); }
            tx.execute("UPDATE alt_requests SET reserved_bytes=reserved_bytes-$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4",&[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&integer(release)?])?;
        }
        tx.execute(
            "UPDATE alt_blobs SET state='stored',accounted=true WHERE repository=$1 AND blake3=$2",
            &[repository, &entry.blake3],
        )?;
        if let Some(oid) = &entry.git_oid {
            let kind = git_kind(entry.kind)?;
            // Git hash and structure are checked from actual pinned bytes in
            // graph validation, before any catalog entry is used for publication.
            if let Some(previous) = tx.query_opt(
                "SELECT kind,blake3 FROM alt_objects WHERE repository=$1 AND oid=$2",
                &[repository, oid],
            )? {
                if previous.get::<_, String>(0) != kind
                    || previous.get::<_, String>(1) != entry.blake3
                {
                    return Err(Error::Integrity("Git identity collision"));
                }
            } else {
                tx.execute(
                    "INSERT INTO alt_objects(repository,oid,kind,blake3) VALUES($1,$2,$3,$4)",
                    &[repository, oid, &kind, &entry.blake3],
                )?;
            }
        }
        if !selection.verified && !selection.entries.is_empty() {
            crate::cloud_quota::accept(&mut tx, &self.spec, key, entry.bytes)?;
        }
        if tx.execute("UPDATE alt_request_content SET verified=true WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 AND blake3=$5", &[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&entry.blake3])? != 1 {
            return Err(Error::Precondition("request content selection missing"));
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
}
pub(crate) fn request_row(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
) -> Result<Option<postgres::Row>> {
    Ok(tx.query_opt("SELECT * FROM alt_requests WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 FOR UPDATE",&[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?)
}
pub(crate) fn active(row: &postgres::Row, time: u64) -> Result<()> {
    if !matches!(
        row.get::<_, String>("state").as_str(),
        "receiving" | "ready"
    ) || unsigned(row.get("expires_at"))? <= time
    {
        return Err(Error::Precondition("request terminal or expired"));
    }
    Ok(())
}
pub(crate) fn check_key(
    key: &RequestKey,
    scope: &alt_remote_protocol::Scope,
    actor: &str,
) -> Result<()> {
    if &key.scope != scope || key.actor_id != actor {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
pub(crate) fn upload_pin(key: &RequestKey) -> String {
    format!(
        "upload:{}",
        alt_hash::blake3::hash(&alt_json::to_vec(key).expect("closed key schema")).to_hex()
    )
}
pub(crate) fn pin(
    tx: &mut Transaction<'_>,
    repository: &str,
    id: &str,
    digest: &str,
    expires: Option<u64>,
) -> Result<()> {
    let expiry = expires.map(integer).transpose()?;
    tx.execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES($1,$2,$3,$4) ON CONFLICT(repository,pin_id,blake3) DO UPDATE SET expires_at=excluded.expires_at WHERE alt_pins.expires_at IS DISTINCT FROM excluded.expires_at",&[&repository,&id,&digest,&expiry])?;
    Ok(())
}
/// Deduplicate equal canonical bytes across roles before a set-wise upsert.
/// The caller already holds the same repository lock as the scalar pin path.
pub(crate) fn pin_many(
    tx: &mut Transaction<'_>,
    repository: &str,
    id: &str,
    digests: &[&str],
    expires: Option<u64>,
) -> Result<()> {
    let expiry = expires.map(integer).transpose()?;
    let unique: std::collections::BTreeSet<_> = digests.iter().copied().collect();
    let unique: Vec<_> = unique.into_iter().collect();
    for batch in unique.chunks(crate::database::BATCH_ROWS) {
        tx.execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) SELECT $1,$2,digest,$4 FROM unnest($3::text[]) AS content(digest) ON CONFLICT(repository,pin_id,blake3) DO UPDATE SET expires_at=excluded.expires_at WHERE alt_pins.expires_at IS DISTINCT FROM excluded.expires_at", &[&repository,&id,&batch,&expiry])?;
    }
    Ok(())
}
pub(crate) fn git_kind(kind: ContentKind) -> Result<&'static str> {
    match kind {
        ContentKind::Blob => Ok("blob"),
        ContentKind::Tree => Ok("tree"),
        ContentKind::Commit => Ok("commit"),
        ContentKind::Tag => Ok("tag"),
        _ => Err(Error::Integrity("non-Git content kind")),
    }
}

/// An agent cannot inspect or mutate another session's private request merely
/// because both are attributed to the same human owner.
pub(crate) fn check_request_attribution(
    row: &postgres::Row,
    auth: &alt_repository_service::Authorization,
) -> Result<()> {
    if auth.attribution.kind == alt_remote_protocol::ActorKind::Agent {
        let publication: Publication = row.get::<_, Jsonb>("publication").read()?;
        if publication.attribution != auth.attribution {
            return Err(Error::Unauthorized);
        }
    }
    Ok(())
}

/// This index is derived once from the validated immutable manifest. A digest
/// can be a whole entry in several roles and a chunk at the same time.
struct SelectedContent<'a> {
    bytes: u64,
    entries: Vec<&'a ContentEntry>,
    chunk: bool,
    /// some entry with these bytes, or made of this chunk, is not a Git object the
    /// segments already hold
    needed: bool,
}
impl SelectedContent<'_> {
    /// nothing has to be uploaded for these bytes
    fn settled(&self) -> bool {
        !self.needed
    }
}
fn selected_contents<'a>(
    manifest: &'a Manifest,
    segments: &alt_pack_segment::SegmentSet,
) -> Result<BTreeMap<String, SelectedContent<'a>>> {
    let mut selected = BTreeMap::new();
    let blank = |bytes| SelectedContent {
        bytes,
        entries: Vec::new(),
        chunk: false,
        needed: false,
    };
    for entry in &manifest.entries {
        let needed = !crate::pack_native::holds_entry(segments, entry)?;
        let item = selected
            .entry(entry.blake3.clone())
            .or_insert_with(|| blank(entry.bytes));
        if item.bytes != entry.bytes {
            return Err(Error::Integrity("conflicting content sizes"));
        }
        item.entries.push(entry);
        item.needed |= needed;
        for chunk in &entry.chunks {
            let item = selected
                .entry(chunk.blake3.clone())
                .or_insert_with(|| blank(chunk.bytes));
            if item.bytes != chunk.bytes {
                return Err(Error::Integrity("conflicting content sizes"));
            }
            item.chunk = true;
            item.needed |= needed;
        }
    }
    Ok(selected)
}

/// Lock current mutable request state without transferring its full manifest.
/// Callers still check current authority, attribution, scope and expiry.
pub(crate) fn upload_request_row(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
) -> Result<Option<postgres::Row>> {
    Ok(tx.query_opt("SELECT state,expires_at,publication,reserved_bytes FROM alt_requests WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 FOR UPDATE", &[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?)
}
pub(crate) struct UploadSelection {
    pub entries: Vec<ContentEntry>,
    pub chunk_bytes: Option<u64>,
    pub verified: bool,
}
pub(crate) fn upload_selection(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
    digest: &str,
) -> Result<Option<UploadSelection>> {
    tx.query_opt("SELECT selected_entries,chunk_bytes,verified FROM alt_request_content WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 AND blake3=$5", &[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&digest])?
        .map(|row| Ok(UploadSelection {
            entries: row.get::<_, Jsonb>(0).read()?,
            chunk_bytes: row.get::<_, Option<i64>>(1).map(unsigned).transpose()?,
            verified: row.get(2),
        })).transpose()
}
