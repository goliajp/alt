use crate::store::{authorize, instance, integer, now, snapshot, unsigned};
use crate::{Credential, DurableBytes, Error, Result, Store};
use alt_remote_protocol::{
    ContentEntry, ContentKind, Manifest, Publication, RequestKey, RequestObservation, RequestState,
};
use alt_repository_service::authorize_intent;
use postgres::Transaction;
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
        manifest.validate(self.limits)?;
        if manifest.object_format != publication.object_format
            || manifest.id(self.limits)? != publication.content_manifest
        {
            return Err(Error::Integrity("transfer manifest identity"));
        }
        if ttl_ms == 0 || ttl_ms > 86_400_000 {
            return Err(Error::Limit("upload lifetime"));
        }
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
            return Ok(serde_json::from_value(row.get("result"))?);
        }
        let expires = time
            .checked_add(ttl_ms)
            .ok_or(Error::Limit("upload lifetime"))?;
        let mut contents = BTreeMap::new();
        for entry in &manifest.entries {
            if let Some(previous) = contents.insert(entry.blake3.clone(), entry.bytes)
                && previous != entry.bytes
            {
                return Err(Error::Integrity("conflicting content sizes"));
            }
        }
        let repository = &publication.key.scope.repository_instance_id;
        let mut reservation = 0u64;
        for (digest, bytes) in &contents {
            let row = tx.query_opt(
                "SELECT bytes,state FROM alt_blobs WHERE repository=$1 AND blake3=$2",
                &[repository, digest],
            )?;
            match row {
                Some(row) => {
                    if unsigned(row.get(0))? != *bytes {
                        return Err(Error::Integrity("content size collision"));
                    }
                    let state: String = row.get(1);
                    if state == "deleting" {
                        return Err(Error::Precondition(
                            "content GC in progress; retry same intent",
                        ));
                    }
                    if state != "stored" {
                        reservation = reservation
                            .checked_add(*bytes)
                            .ok_or(Error::Limit("upload reservation"))?;
                    }
                }
                None => {
                    reservation = reservation
                        .checked_add(*bytes)
                        .ok_or(Error::Limit("upload reservation"))?;
                    tx.execute("INSERT INTO alt_blobs(repository,blake3,bytes,state,created_at) VALUES($1,$2,$3,'staging',$4)",&[repository,digest,&integer(*bytes)?,&integer(time)?])?;
                }
            }
        }
        if tx.execute("UPDATE alt_repositories SET reserved_bytes=reserved_bytes+$2 WHERE id=$1 AND $2<=quota_bytes-stored_bytes-reserved_bytes",&[repository,&integer(reservation)?])?!=1 { return Err(Error::Limit("repository quota")); }
        let state = RequestState::Receiving {
            digest: digest.clone(),
            expires_at_unix_ms: expires,
        };
        tx.execute("INSERT INTO alt_requests(repository,epoch,actor_id,request_id,digest,publication,manifest,state,result,expires_at,reserved_bytes) VALUES($1,$2,$3,$4,$5,$6,$7,'receiving',$8,$9,$10)",&[repository,&integer(publication.key.scope.recovery_epoch)?,&publication.key.actor_id,&publication.key.request_id,&digest,&serde_json::to_value(publication)?,&serde_json::to_value(manifest)?,&serde_json::to_value(&state)?,&integer(expires)?,&integer(reservation)?])?;
        for digest in contents.keys() {
            let published = tx.query_opt("SELECT 1 FROM alt_pins p JOIN alt_blobs b ON b.repository=p.repository AND b.blake3=p.blake3 WHERE p.repository=$1 AND p.blake3=$2 AND p.expires_at IS NULL AND b.state='stored' LIMIT 1", &[repository, digest])?.is_some();
            tx.execute("INSERT INTO alt_request_content(repository,epoch,actor_id,request_id,blake3,verified) VALUES($1,$2,$3,$4,$5,$6)", &[repository,&integer(publication.key.scope.recovery_epoch)?,&publication.key.actor_id,&publication.key.request_id,digest,&published])?;
            pin(
                &mut tx,
                repository,
                &upload_pin(&publication.key),
                digest,
                Some(expires),
            )?;
        }
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
        let result = match request_row(&mut tx, key)? {
            Some(row) => RequestObservation::Recorded {
                key: key.clone(),
                result: Box::new(serde_json::from_value(row.get("result"))?),
            },
            None if key.scope.recovery_epoch == current.scope.recovery_epoch => {
                RequestObservation::NotSeen { key: key.clone() }
            }
            None => RequestObservation::Unknown { key: key.clone() },
        };
        tx.commit()?;
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
        self.bytes.verify_content_chunks(proof.scope(), entry)?;
        // Verify the Git mapping before inserting it, so a malicious upload
        // cannot poison a future legitimate OID with unrelated payload bytes.
        if let Some(oid) = &entry.git_oid {
            let oid = oid.parse().map_err(|_| Error::Integrity("Git object id"))?;
            let kind = git_kind(entry.kind)?
                .parse()
                .map_err(|_| Error::Integrity("Git object kind"))?;
            let reader = self
                .bytes
                .read(proof.scope(), proof.blake3(), proof.bytes())?;
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
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(key, &current.scope, &auth.actor_id)?;
        let row =
            request_row(&mut tx, key)?.ok_or(Error::Precondition("request not registered"))?;
        active(&row, now(&mut tx)?)?;
        let manifest: Manifest = serde_json::from_value(row.get("manifest"))?;
        if !manifest.entries.contains(entry)
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
            if tx.execute("UPDATE alt_repositories SET stored_bytes=stored_bytes+$2,reserved_bytes=reserved_bytes-$3 WHERE id=$1 AND $2::bigint-$3::bigint<=quota_bytes-stored_bytes-reserved_bytes",&[repository,&integer(entry.bytes)?,&integer(release)?])?!=1 { return Err(Error::Limit("repository quota")); }
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
        blake3::hash(&serde_json::to_vec(key).expect("closed key schema")).to_hex()
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
    tx.execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES($1,$2,$3,$4) ON CONFLICT(repository,pin_id,blake3) DO UPDATE SET expires_at=excluded.expires_at",&[&repository,&id,&digest,&expiry])?;
    Ok(())
}
fn git_kind(kind: ContentKind) -> Result<&'static str> {
    match kind {
        ContentKind::Blob => Ok("blob"),
        ContentKind::Tree => Ok("tree"),
        ContentKind::Commit => Ok("commit"),
        ContentKind::Tag => Ok("tag"),
        _ => Err(Error::Integrity("non-Git content kind")),
    }
}
