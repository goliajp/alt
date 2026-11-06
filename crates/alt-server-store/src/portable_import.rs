//! Receiving a portable snapshot never creates a current publication, grant or
//! public object. Immutable intent, quota and temporary roots survive restart.
use crate::store::{authorize, instance, integer, now, require_admin, snapshot, unsigned};
use crate::{Credential, DurableBytes, Error, InstanceSpec, Result, Store};
pub use alt_portable::http::ImportState as PortableImportState;
use alt_portable::{Blob, Envelope, ImportIntent, ImportReceipt, PackageLimits};
use alt_remote_protocol::{ActorKind, RequestKey, Scope};
use alt_repository_service::{Authorization, RepositorySnapshot};
use postgres::{Row, Transaction};
fn pin_id(digest: &str) -> String {
    format!("portable-import:{digest}")
}
fn row(tx: &mut Transaction<'_>, key: &RequestKey) -> Result<Option<Row>> {
    Ok(tx.query_opt(
        "SELECT * FROM alt_portable_imports WHERE repository=$1 AND import_id=$2 FOR UPDATE",
        &[&key.scope.repository_instance_id, &key.request_id],
    )?)
}
fn authorize_read(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
    key: &RequestKey,
) -> Result<(RepositorySnapshot, Authorization)> {
    key.validate()?;
    let current = snapshot(tx, spec, &key.scope.repository_instance_id)?;
    require_admin(tx, spec, credential)?;
    let auth = authorize(tx, spec, credential, &current)?;
    if key.scope.installation_id != spec.installation_id || auth.actor_id != key.actor_id {
        return Err(Error::Unauthorized);
    }
    Ok((current, auth))
}
fn authorize_write(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &RequestKey,
    current: &RepositorySnapshot,
    auth: &Authorization,
    expected_import: Option<&str>,
) -> Result<()> {
    if current.scope != key.scope
        || auth.attribution.kind != ActorKind::Human
        || !auth.publish_records
        || !auth.grants.iter().any(|g| {
            matches!(&g.refs, alt_remote_protocol::authorization::RefSelector::Namespace(v) if v == "refs/")
                && g.create && g.update && g.delete && g.force && g.paths.is_none()
        })
    {
        return Err(Error::Unauthorized);
    }
    let row = tx.query_one(
        "SELECT writes_enabled,portable_freeze,portable_import FROM alt_repositories WHERE id=$1",
        &[&key.scope.repository_instance_id],
    )?;
    let active: Option<String> = row.get(2);
    if !row.get::<_, bool>(0)
        || row.get::<_, Option<String>>(1).is_some()
        || !crate::cloud::writes_enabled(tx, spec)?
    {
        return Err(Error::Unauthorized);
    }
    if active.as_deref() != expected_import {
        return Err(Error::Precondition("repository portable import changed"));
    }
    Ok(())
}
fn empty(tx: &mut Transaction<'_>, repository: &str) -> Result<()> {
    let empty: bool = tx.query_one("SELECT transaction_sequence=0 AND NOT EXISTS(SELECT 1 FROM alt_refs WHERE repository=$1) AND NOT EXISTS(SELECT 1 FROM alt_task_records WHERE repository=$1) AND NOT EXISTS(SELECT 1 FROM alt_requests WHERE repository=$1) FROM alt_repositories WHERE id=$1", &[&repository])?.get(0);
    if !empty {
        return Err(Error::Precondition(
            "portable import requires a new empty repository",
        ));
    }
    Ok(())
}
fn mappings(tx: &mut Transaction<'_>, intent: &ImportIntent) -> Result<()> {
    for actor in intent
        .mappings
        .iter()
        .filter_map(|m| m.target_actor.as_ref())
    {
        // This is an explicit historical link, not a capability. An identity
        // may remain known after its access is revoked; no credential is copied.
        let known: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_web_accounts WHERE actor_id=$1) OR EXISTS(SELECT 1 FROM alt_cloud_accounts WHERE actor_id=$1) OR EXISTS(SELECT 1 FROM alt_credentials WHERE actor_id=$1 AND attribution->>'kind'='human')", &[actor])?.get(0);
        if !known {
            return Err(Error::Precondition(
                "portable mapping target identity not found",
            ));
        }
    }
    Ok(())
}
fn same(row: &Row, intent: &ImportIntent, limits: PackageLimits) -> Result<()> {
    let recorded: ImportIntent = serde_json::from_value(row.get("intent"))?;
    if recorded != *intent || row.get::<_, String>("digest") != intent.digest(limits)? {
        return Err(Error::Precondition(
            "import ID reused with different parameters",
        ));
    }
    Ok(())
}
fn active(row: &Row, key: &RequestKey, time: u64) -> Result<()> {
    if row.get::<_, String>("state") != "receiving"
        || unsigned(row.get("epoch"))? != key.scope.recovery_epoch
        || row.get::<_, String>("actor_id") != key.actor_id
        || unsigned(row.get("expires_at"))? <= time
    {
        return Err(Error::Precondition("portable import terminal or expired"));
    }
    Ok(())
}
fn state(
    tx: &mut Transaction<'_>,
    row: &Row,
    key: &RequestKey,
    current: &Scope,
) -> Result<PortableImportState> {
    let recorded: ImportIntent = serde_json::from_value(row.get("intent"))?;
    if recorded.key != *key {
        return Err(Error::Precondition("portable import identity mismatch"));
    }
    let digest: String = row.get("digest");
    Ok(match row.get::<_, String>("state").as_str() {
        "committed" => PortableImportState::Committed {
            receipt: Box::new(serde_json::from_value(row.get("receipt"))?),
        },
        "abandoned" => PortableImportState::Abandoned { digest },
        "receiving" if key.scope != *current => PortableImportState::Unknown { key: key.clone() },
        "receiving" => {
            let envelope: Envelope = serde_json::from_value(row.get("envelope"))?;
            let verified: std::collections::BTreeSet<String> = tx.query("SELECT c.blake3 FROM alt_portable_import_content c JOIN alt_blobs b ON b.repository=c.repository AND b.blake3=c.blake3 WHERE c.repository=$1 AND c.import_id=$2 AND c.verified AND b.state='stored'", &[&key.scope.repository_instance_id,&key.request_id])?.iter().map(|r| r.get(0)).collect();
            PortableImportState::Receiving {
                digest,
                expires_at_unix_ms: unsigned(row.get("expires_at"))?,
                missing: envelope
                    .catalog
                    .blobs
                    .into_iter()
                    .filter(|b| !verified.contains(&b.blake3))
                    .collect(),
            }
        }
        _ => return Err(Error::Integrity("portable import state")),
    })
}
impl Store {
    pub fn begin_portable_import(
        &mut self,
        credential: &Credential,
        intent: &ImportIntent,
        envelope: &Envelope,
        ttl_ms: u64,
        limits: PackageLimits,
    ) -> Result<PortableImportState> {
        intent.validate_envelope(envelope, limits)?;
        if ttl_ms == 0 || ttl_ms > 86_400_000 {
            return Err(Error::Limit("portable import lifetime"));
        }
        if intent.target_public_key != self.receipt_public_key() {
            return Err(Error::Precondition("portable target key mismatch"));
        }
        let key = &intent.key;
        let digest = intent.digest(limits)?;
        let repository = &key.scope.repository_instance_id;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, auth) = authorize_read(&mut tx, &self.spec, credential, key)?;
        if current.scope != key.scope || current.object_format != envelope.catalog.object_format {
            return Err(Error::Unauthorized);
        }
        if let Some(row) = row(&mut tx, key)? {
            same(&row, intent, limits)?;
            let result = state(&mut tx, &row, key, &current.scope)?;
            tx.commit()?;
            return Ok(result);
        }
        authorize_write(&mut tx, &self.spec, key, &current, &auth, None)?;
        empty(&mut tx, repository)?;
        mappings(&mut tx, intent)?;
        let time = now(&mut tx)?;
        let expires = time
            .checked_add(ttl_ms)
            .ok_or(Error::Limit("portable import lifetime"))?;
        let mut reservation = 0u64;
        let mut ingress = 0u64;
        for blob in &envelope.catalog.blobs {
            ingress = ingress
                .checked_add(blob.bytes)
                .ok_or(Error::Limit("portable ingress reservation"))?;
            let old = tx.query_opt(
                "SELECT bytes,state,accounted FROM alt_blobs WHERE repository=$1 AND blake3=$2",
                &[repository, &blob.blake3],
            )?;
            if let Some(old) = old {
                if unsigned(old.get(0))? != blob.bytes {
                    return Err(Error::Integrity("portable content size collision"));
                }
                if old.get::<_, String>(1) == "deleting" {
                    return Err(Error::Precondition("portable content GC in progress"));
                }
                if !old.get::<_, bool>(2) {
                    reservation = reservation
                        .checked_add(blob.bytes)
                        .ok_or(Error::Limit("portable storage reservation"))?;
                }
            } else {
                reservation = reservation
                    .checked_add(blob.bytes)
                    .ok_or(Error::Limit("portable storage reservation"))?;
                tx.execute("INSERT INTO alt_blobs(repository,blake3,bytes,state,created_at) VALUES($1,$2,$3,'staging',$4)", &[repository,&blob.blake3,&integer(blob.bytes)?,&integer(time)?])?;
            }
        }
        if tx.execute("UPDATE alt_repositories SET reserved_bytes=reserved_bytes+$2,portable_import=$3 WHERE id=$1 AND $2<=quota_bytes-stored_bytes-reserved_bytes", &[repository,&integer(reservation)?,&key.request_id])? != 1 {
            return Err(Error::Limit("repository quota"));
        }
        tx.execute("INSERT INTO alt_portable_imports(repository,import_id,actor_id,epoch,digest,intent,envelope,state,expires_at,reserved_bytes,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,'receiving',$8,$9,$10)", &[repository,&key.request_id,&key.actor_id,&integer(key.scope.recovery_epoch)?,&digest,&serde_json::to_value(intent)?,&serde_json::to_value(envelope)?,&integer(expires)?,&integer(reservation)?,&integer(time)?])?;
        for blob in &envelope.catalog.blobs {
            tx.execute("INSERT INTO alt_portable_import_content(repository,import_id,blake3) VALUES($1,$2,$3)", &[repository,&key.request_id,&blob.blake3])?;
            crate::requests::pin(
                &mut tx,
                repository,
                &pin_id(&digest),
                &blob.blake3,
                Some(expires),
            )?;
        }
        crate::cloud_quota::reserve_portable(&mut tx, &self.spec, key, ingress)?;
        crate::browser::audit_details(
            &mut tx,
            &key.actor_id,
            "portable.import.started",
            repository,
            &serde_json::json!({"intent_digest":digest,"source":intent.source,"import_id":key.request_id}),
        )?;
        let result = PortableImportState::Receiving {
            digest,
            expires_at_unix_ms: expires,
            missing: envelope.catalog.blobs.clone(),
        };
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
    pub fn query_portable_import(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<PortableImportState> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, _) = authorize_read(&mut tx, &self.spec, credential, key)?;
        let result = match row(&mut tx, key)? {
            Some(row) => state(&mut tx, &row, key, &current.scope)?,
            None if current.scope == key.scope => PortableImportState::NotSeen { key: key.clone() },
            None => PortableImportState::Unknown { key: key.clone() },
        };
        tx.commit()?;
        Ok(result)
    }
    pub fn register_portable_content(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        proof: &DurableBytes,
    ) -> Result<()> {
        if proof.scope() != &key.scope {
            return Err(Error::Integrity("portable upload scope"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, auth) = authorize_read(&mut tx, &self.spec, credential, key)?;
        authorize_write(
            &mut tx,
            &self.spec,
            key,
            &current,
            &auth,
            Some(&key.request_id),
        )?;
        let row = row(&mut tx, key)?.ok_or(Error::Precondition("portable import not found"))?;
        let time = now(&mut tx)?;
        active(&row, key, time)?;
        let envelope: Envelope = serde_json::from_value(row.get("envelope"))?;
        let selected = envelope
            .catalog
            .blobs
            .iter()
            .any(|b| b.blake3 == proof.blake3() && b.bytes == proof.bytes());
        if !selected {
            return Err(Error::Integrity("portable upload outside selection"));
        }
        let repository = &key.scope.repository_instance_id;
        let blob = tx.query_opt("SELECT c.verified,b.state,b.accounted,b.bytes FROM alt_portable_import_content c JOIN alt_blobs b ON b.repository=c.repository AND b.blake3=c.blake3 JOIN alt_pins p ON p.repository=b.repository AND p.blake3=b.blake3 AND p.pin_id=$4 WHERE c.repository=$1 AND c.import_id=$2 AND c.blake3=$3 AND p.expires_at>$5", &[repository,&key.request_id,&proof.blake3(),&pin_id(&row.get::<_,String>("digest")),&integer(time)?])?.ok_or(Error::Precondition("portable upload pin missing"))?;
        if blob.get::<_, String>(1) == "deleting" || unsigned(blob.get(3))? != proof.bytes() {
            return Err(Error::Precondition("portable upload fenced by GC"));
        }
        self.bytes.check_proof(proof)?;
        if !blob.get::<_, bool>(2) {
            let remaining = unsigned(row.get("reserved_bytes"))?;
            let release = remaining.min(proof.bytes());
            if tx.execute("UPDATE alt_repositories SET stored_bytes=stored_bytes+$2,reserved_bytes=reserved_bytes-$3 WHERE id=$1 AND $2::bigint-$3::bigint<=quota_bytes-stored_bytes-reserved_bytes", &[repository,&integer(proof.bytes())?,&integer(release)?])? != 1 {
                return Err(Error::Limit("repository quota"));
            }
            tx.execute("UPDATE alt_portable_imports SET reserved_bytes=reserved_bytes-$3 WHERE repository=$1 AND import_id=$2", &[repository,&key.request_id,&integer(release)?])?;
        }
        tx.execute(
            "UPDATE alt_blobs SET state='stored',accounted=true WHERE repository=$1 AND blake3=$2",
            &[repository, &proof.blake3()],
        )?;
        if !blob.get::<_, bool>(0) {
            crate::cloud_quota::accept_portable(&mut tx, &self.spec, key, proof.bytes())?;
            tx.execute("UPDATE alt_portable_import_content SET verified=true WHERE repository=$1 AND import_id=$2 AND blake3=$3", &[repository,&key.request_id,&proof.blake3()])?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
    /// Explicit abandonment releases only this import's reservations and pins.
    /// Already accepted ingress remains a historical usage fact.
    pub fn abandon_portable_import(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, _) = authorize_read(&mut tx, &self.spec, credential, key)?;
        if current.scope != key.scope {
            return Err(Error::Unauthorized);
        }
        let row = row(&mut tx, key)?.ok_or(Error::Precondition("portable import not found"))?;
        let recorded: ImportIntent = serde_json::from_value(row.get("intent"))?;
        if recorded.key != *key {
            return Err(Error::Unauthorized);
        }
        match row.get::<_, String>("state").as_str() {
            "committed" => {
                return Err(Error::Precondition(
                    "committed portable import cannot be abandoned",
                ));
            }
            "abandoned" => {
                tx.commit()?;
                return Ok(());
            }
            "receiving" => {}
            _ => return Err(Error::Integrity("portable import state")),
        }
        let repository = &key.scope.repository_instance_id;
        let active: Option<String> = tx
            .query_one(
                "SELECT portable_import FROM alt_repositories WHERE id=$1",
                &[repository],
            )?
            .get(0);
        if active.as_deref() != Some(&key.request_id) {
            return Err(Error::Precondition("portable import lock changed"));
        }
        tx.execute("UPDATE alt_repositories SET reserved_bytes=reserved_bytes-$2,portable_import=NULL WHERE id=$1", &[repository,&row.get::<_,i64>("reserved_bytes")])?;
        crate::cloud_quota::release_portable(&mut tx, key)?;
        tx.execute("UPDATE alt_portable_imports SET state='abandoned',reserved_bytes=0 WHERE repository=$1 AND import_id=$2", &[repository,&key.request_id])?;
        tx.execute(
            "DELETE FROM alt_pins WHERE repository=$1 AND pin_id=$2",
            &[repository, &pin_id(&row.get::<_, String>("digest"))],
        )?;
        crate::browser::audit_details(
            &mut tx,
            &key.actor_id,
            "portable.import.abandoned",
            repository,
            &serde_json::json!({"import_id":key.request_id}),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}

mod activation;
pub use activation::PortableImportWork;

impl Store {
    /// Validate an immutable upload selection before allocating request-body
    /// storage. Registration repeats these checks after durable file creation.
    pub fn selected_portable_upload(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<Blob> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, auth) = authorize_read(&mut tx, &self.spec, credential, key)?;
        authorize_write(
            &mut tx,
            &self.spec,
            key,
            &current,
            &auth,
            Some(&key.request_id),
        )?;
        let row = row(&mut tx, key)?.ok_or(Error::Precondition("portable import not found"))?;
        active(&row, key, now(&mut tx)?)?;
        let envelope: Envelope = serde_json::from_value(row.get("envelope"))?;
        let blob = envelope
            .catalog
            .blobs
            .into_iter()
            .find(|blob| blob.blake3 == digest)
            .ok_or(Error::Unauthorized)?;
        tx.commit()?;
        Ok(blob)
    }
}
