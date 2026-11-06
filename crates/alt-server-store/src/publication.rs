use crate::requests::{active, check_key, pin, request_row, upload_pin};
use crate::store::{authorize, instance, integer, now, snapshot, unsigned};
use crate::{ByteStore, Credential, Error, Result, Store};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_remote_protocol::{Manifest, Publication, Receipt, ReceiptBody, RequestKey, RequestState};
use alt_repository_service::{ObjectReader, ObjectSource, VerifiedGraph, validate_publication};
use ed25519_dalek::Signer;
use postgres::Transaction;
use std::collections::BTreeMap;

/// A leased worker snapshot. The fence is rechecked for every transition; a
/// stale worker can leave orphan bytes but cannot publish or change a result.
pub struct PublicationWork {
    pub(crate) key: RequestKey,
    pub(crate) fence: u64,
    pub(crate) publication: Publication,
    pub(crate) manifest: Manifest,
    pub(crate) source: PinnedSource,
}
impl PublicationWork {
    pub fn publication(&self) -> &Publication {
        &self.publication
    }
    pub fn source(&self) -> &impl ObjectSource {
        &self.source
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn artifacts(&self) -> &impl alt_task_service::ArtifactSource {
        &self.source
    }
    pub fn roots(&self) -> Result<Vec<ObjectId>> {
        self.publication
            .refs
            .iter()
            .flat_map(|update| [&update.expected, &update.new])
            .flatten()
            .map(|oid| {
                oid.parse()
                    .map_err(|_| Error::Integrity("publication object id"))
            })
            .collect()
    }
}
pub(crate) struct PinnedSource {
    bytes: ByteStore,
    scope: alt_remote_protocol::Scope,
    pub(crate) objects: BTreeMap<ObjectId, (ObjectKind, String, u64)>,
    artifacts: BTreeMap<String, u64>,
}
impl alt_task_service::ArtifactSource for PinnedSource {
    fn open(
        &self,
        artifact: &alt_task::Artifact,
    ) -> alt_task_service::Result<Box<dyn std::io::Read + '_>> {
        if self.artifacts.get(&artifact.blake3) != Some(&artifact.bytes) {
            return Err(alt_task_service::Error::Integrity(
                "artifact outside selected publication",
            ));
        }
        let file = self
            .bytes
            .read(&self.scope, &artifact.blake3, artifact.bytes)
            .map_err(|_| alt_task_service::Error::Integrity("selected artifact unavailable"))?;
        Ok(Box::new(file))
    }
}
impl ObjectSource for PinnedSource {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        let Some((kind, digest, bytes)) = self.objects.get(&oid) else {
            return Ok(None);
        };
        let file = self
            .bytes
            .read(&self.scope, digest, *bytes)
            .map_err(|e| match e {
                Error::Io(error) => alt_repository_service::Error::Io(error),
                _ => alt_repository_service::Error::Integrity("pinned bytes unavailable"),
            })?;
        Ok(Some(ObjectReader {
            kind: *kind,
            bytes: *bytes,
            reader: Box::new(file),
        }))
    }
}
impl Store {
    pub fn acquire_publication(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        lease_ms: u64,
        max_objects: usize,
    ) -> Result<PublicationWork> {
        self.acquire_work(credential, key, lease_ms, max_objects, false)
    }
    /// A current reviewer may inspect an already-ready selected publication.
    /// This handle cannot publish on behalf of the originating actor.
    pub fn acquire_review(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        lease_ms: u64,
        max_objects: usize,
    ) -> Result<PublicationWork> {
        self.acquire_work(credential, key, lease_ms, max_objects, true)
    }
    fn acquire_work(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        lease_ms: u64,
        max_objects: usize,
        review: bool,
    ) -> Result<PublicationWork> {
        if lease_ms == 0 || lease_ms > 300_000 || max_objects == 0 {
            return Err(Error::Limit("worker lease or object bound"));
        }
        let max_objects = max_objects.min(self.limits.graph_objects);
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        key.validate()?;
        if review {
            if key.scope != current.scope {
                return Err(Error::Unauthorized);
            }
            crate::tasks::reviewer(&mut tx, &self.spec, credential, &current)?;
        } else {
            check_key(key, &current.scope, &auth.actor_id)?;
        }
        let row =
            request_row(&mut tx, key)?.ok_or(Error::Precondition("request not registered"))?;
        let time = now(&mut tx)?;
        active(&row, time)?;
        if review && row.get::<_, String>("state") != "ready" {
            return Err(Error::Precondition("publication not ready for review"));
        }
        if unsigned(row.get("worker_lease_until"))? > time {
            return Err(Error::Precondition("publication worker already leased"));
        }
        let publication: Publication = serde_json::from_value(row.get("publication"))?;
        if !review {
            alt_repository_service::authorize_intent(
                &publication,
                &current,
                &auth,
                self.limits,
                time,
            )?;
        }
        let manifest: Manifest = serde_json::from_value(row.get("manifest"))?;
        manifest.validate(self.limits)?;
        let repository = &key.scope.repository_instance_id;
        for entry in &manifest.entries {
            if tx.query_opt("SELECT 1 FROM alt_request_content WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 AND blake3=$5 AND verified", &[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&entry.blake3])?.is_none() {
                return Err(Error::Precondition("selected content not uploaded or published"));
            }
            if let Some(oid) = &entry.git_oid {
                let object = tx
                    .query_opt(
                        "SELECT kind,blake3 FROM alt_objects WHERE repository=$1 AND oid=$2",
                        &[repository, oid],
                    )?
                    .ok_or(Error::Precondition("Git mapping not uploaded"))?;
                let kind = match entry.kind {
                    alt_remote_protocol::ContentKind::Blob => "blob",
                    alt_remote_protocol::ContentKind::Tree => "tree",
                    alt_remote_protocol::ContentKind::Commit => "commit",
                    alt_remote_protocol::ContentKind::Tag => "tag",
                    _ => return Err(Error::Integrity("Git mapping kind")),
                };
                if object.get::<_, String>(0) != kind || object.get::<_, String>(1) != entry.blake3
                {
                    return Err(Error::Integrity("Git mapping selection"));
                }
            }
            let blob = tx
                .query_opt(
                    "SELECT bytes,state FROM alt_blobs WHERE repository=$1 AND blake3=$2",
                    &[repository, &entry.blake3],
                )?
                .ok_or(Error::Precondition("upload incomplete"))?;
            if unsigned(blob.get(0))? != entry.bytes || blob.get::<_, String>(1) != "stored" {
                return Err(Error::Precondition("upload incomplete"));
            }
        }
        let expires = unsigned(row.get("expires_at"))?;
        let lease_until = time
            .checked_add(lease_ms)
            .ok_or(Error::Limit("worker lifetime"))?
            .min(expires);
        let pin_id = upload_pin(key);
        let rows=tx.query("SELECT o.oid,o.kind,o.blake3,b.bytes FROM alt_objects o JOIN alt_blobs b ON b.repository=o.repository AND b.blake3=o.blake3 WHERE o.repository=$1 AND b.state='stored' AND EXISTS (SELECT 1 FROM alt_pins p WHERE p.repository=o.repository AND p.blake3=o.blake3 AND (p.expires_at IS NULL OR (p.pin_id=$2 AND p.expires_at>$3 AND EXISTS(SELECT 1 FROM alt_request_content c WHERE c.repository=o.repository AND c.epoch=$5 AND c.actor_id=$6 AND c.request_id=$7 AND c.blake3=o.blake3 AND c.verified)))) ORDER BY o.oid LIMIT $4",&[repository,&pin_id,&integer(time)?,&integer((max_objects as u64).saturating_add(1))?,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?;
        if rows.len() > max_objects {
            return Err(Error::Limit("repository graph objects"));
        }
        let mut objects = BTreeMap::new();
        for row in rows {
            let oid: String = row.get(0);
            let kind: String = row.get(1);
            let digest: String = row.get(2);
            let bytes = unsigned(row.get(3))?;
            let oid = oid
                .parse()
                .map_err(|_| Error::Integrity("catalog object id"))?;
            let kind = kind
                .parse()
                .map_err(|_| Error::Integrity("catalog object kind"))?;
            pin(&mut tx, repository, &pin_id, &digest, Some(expires))?;
            objects.insert(oid, (kind, digest, bytes));
        }
        let fence=unsigned(tx.query_one("UPDATE alt_requests SET fence=fence+1,worker_lease_until=$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 RETURNING fence",&[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&integer(lease_until)?])?.get(0))?;
        let artifacts = manifest
            .entries
            .iter()
            .map(|e| (e.blake3.clone(), e.bytes))
            .collect();
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(PublicationWork {
            key: key.clone(),
            fence,
            publication,
            manifest,
            source: PinnedSource {
                bytes: self.bytes.clone(),
                scope: key.scope.clone(),
                objects,
                artifacts,
            },
        })
    }
    pub fn renew_publication(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        lease_ms: u64,
    ) -> Result<()> {
        if lease_ms == 0 || lease_ms > 300_000 {
            return Err(Error::Limit("worker lease"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(&work.key, &current.scope, &auth.actor_id)?;
        let row = worker(&mut tx, work)?;
        crate::requests::check_request_attribution(&row, &auth)?;
        let expiry = now(&mut tx)?
            .checked_add(lease_ms)
            .ok_or(Error::Limit("worker lease"))?
            .min(unsigned(row.get("expires_at"))?);
        tx.execute("UPDATE alt_requests SET worker_lease_until=$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[&work.key.scope.repository_instance_id, &integer(work.key.scope.recovery_epoch)?, &work.key.actor_id, &work.key.request_id, &integer(expiry)?])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
    /// Persist a validation failure without erasing its original request ID.
    /// Only the current fenced worker can decide this terminal result.
    pub fn reject_publication(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        code: alt_remote_protocol::ErrorCode,
        message: &str,
    ) -> Result<RequestState> {
        if message.is_empty() || message.len() > 4096 {
            return Err(Error::Limit("rejection message"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(&work.key, &current.scope, &auth.actor_id)?;
        let row = worker(&mut tx, work)?;
        crate::requests::check_request_attribution(&row, &auth)?;
        let state = RequestState::Rejected {
            digest: row.get("digest"),
            failure: alt_remote_protocol::Failure {
                code,
                message: message.into(),
                request: Some(work.key.clone()),
                retry: alt_remote_protocol::Retry::NewIntent,
            },
        };
        finish(
            &mut tx,
            &work.key,
            "rejected",
            &state,
            unsigned(row.get("reserved_bytes"))?,
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(state)
    }
    pub fn mark_ready(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
    ) -> Result<()> {
        self.mark_ready_checked(credential, work, graph, None)
    }
    pub fn mark_ready_with_tasks(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: &alt_task_service::VerifiedTaskGraph,
    ) -> Result<()> {
        self.mark_ready_checked(credential, work, graph, Some(tasks))
    }
    fn mark_ready_checked(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: Option<&alt_task_service::VerifiedTaskGraph>,
    ) -> Result<()> {
        crate::tasks::check_selection(work, tasks)?;
        // Reusing published bytes skips upload registration, but it must not
        // skip this request's chunk claims. Do the bounded I/O before SQL locks.
        for entry in &work.manifest.entries {
            self.bytes.verify_content_chunks(&work.key.scope, entry)?;
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(&work.key, &current.scope, &auth.actor_id)?;
        let row = worker(&mut tx, work)?;
        crate::requests::check_request_attribution(&row, &auth)?;
        for root in work.roots()? {
            graph.node(root)?;
        }
        // Graph validation must have used this worker's authorized object set.
        check_graph_pins(&mut tx, work, graph)?;
        crate::tasks::check_pins(&mut tx, work, graph, tasks)?;
        let state = RequestState::Ready {
            digest: row.get("digest"),
        };
        tx.execute("UPDATE alt_requests SET state='ready',result=$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4",&[&work.key.scope.repository_instance_id,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id,&serde_json::to_value(&state)?])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
    /// Byte graph validation happens outside this short transaction. The
    /// transaction rechecks all mutable authority, CAS, policy and pin fences.
    pub fn publish(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
    ) -> Result<Receipt> {
        self.publish_checked(credential, work, graph, None)
    }
    pub fn publish_with_tasks(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: &alt_task_service::VerifiedTaskGraph,
    ) -> Result<Receipt> {
        self.publish_checked(credential, work, graph, Some(tasks))
    }
    fn publish_checked(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: Option<&alt_task_service::VerifiedTaskGraph>,
    ) -> Result<Receipt> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let mut current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(&work.key, &current.scope, &auth.actor_id)?;
        let row = request_row(&mut tx, &work.key)?
            .ok_or(Error::Precondition("request not registered"))?;
        crate::requests::check_request_attribution(&row, &auth)?;
        if row.get::<_, String>("state") == "committed" {
            let state: RequestState = serde_json::from_value(row.get("result"))?;
            if let RequestState::Committed { receipt } = state {
                return Ok(*receipt);
            }
            return Err(Error::Integrity("committed request result"));
        }
        let _ = worker(&mut tx, work)?;
        if row.get::<_, String>("state") != "ready" {
            return Err(Error::Precondition("request not ready"));
        }
        crate::tasks::check_selection(work, tasks)?;
        crate::tasks::check_pins(&mut tx, work, graph, tasks)?;
        crate::tasks::approve_current(&mut tx, &self.spec, work, tasks, &mut current, self.limits)?;
        let validated = match validate_publication(
            &work.publication,
            &current,
            &auth,
            graph,
            self.limits,
            now(&mut tx)?,
        ) {
            Ok(validated) => validated,
            Err(error) => {
                use alt_remote_protocol::{ErrorCode, Failure, Retry};
                let code = match &error {
                    alt_repository_service::Error::Precondition(_) => ErrorCode::PreconditionFailed,
                    alt_repository_service::Error::Forbidden(_) => ErrorCode::Forbidden,
                    alt_repository_service::Error::Limit(_) => ErrorCode::QuotaExceeded,
                    _ => ErrorCode::IntegrityError,
                };
                let result = RequestState::Rejected {
                    digest: row.get("digest"),
                    failure: Failure {
                        code,
                        message: error.to_string(),
                        request: Some(work.key.clone()),
                        retry: Retry::NewIntent,
                    },
                };
                finish(
                    &mut tx,
                    &work.key,
                    "rejected",
                    &result,
                    unsigned(row.get("reserved_bytes"))?,
                )?;
                tx.commit().map_err(|_| Error::OutcomeUnknown)?;
                return Err(error.into());
            }
        };
        check_graph_pins(&mut tx, work, graph)?;
        crate::change_repair::claim_published_tasks(
            &mut tx,
            &self.spec,
            work,
            tasks,
            &current,
            &auth,
            &row.get::<_, String>("digest"),
        )?;
        let repository = &work.key.scope.repository_instance_id;
        for update in &validated.updates {
            let ref_pin = format!("ref:{}", update.name);
            tx.execute(
                "DELETE FROM alt_pins WHERE repository=$1 AND pin_id=$2",
                &[repository, &ref_pin],
            )?;
            if let Some(oid) = update.new {
                for reachable in graph.reachable(oid)? {
                    let (_, digest, _) = work
                        .source
                        .objects
                        .get(&reachable)
                        .ok_or(Error::Integrity("object outside worker source"))?;
                    pin(&mut tx, repository, &ref_pin, digest, None)?;
                }
                tx.execute("INSERT INTO alt_refs(repository,name,oid) VALUES($1,$2,$3) ON CONFLICT(repository,name) DO UPDATE SET oid=excluded.oid",&[repository,&update.name,&oid.to_string()])?;
            } else {
                tx.execute(
                    "DELETE FROM alt_refs WHERE repository=$1 AND name=$2",
                    &[repository, &update.name],
                )?;
            }
        }
        let sequence=unsigned(tx.query_one("UPDATE alt_repositories SET transaction_sequence=transaction_sequence+1 WHERE id=$1 RETURNING transaction_sequence",&[repository])?.get(0))?;
        crate::tasks::retain(&mut tx, work, graph, tasks, sequence, &validated.digest)?;
        let body = ReceiptBody {
            publication: work.publication.clone(),
            request_digest: validated.digest,
            transaction_sequence: sequence,
            policy_version: validated.policy_version,
            committed_at_unix_ms: now(&mut tx)?,
        };
        let signature = self.signing_key.sign(&body.signing_bytes(self.limits)?);
        let receipt = Receipt {
            body,
            signing_key_id: blake3::hash(self.signing_key.verifying_key().as_bytes())
                .to_hex()
                .to_string(),
            signature: signature
                .to_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        };
        let result = RequestState::Committed {
            receipt: Box::new(receipt.clone()),
        };
        let event = serde_json::json!({"kind":"publication_committed","receipt":receipt});
        tx.execute(
            "INSERT INTO alt_audit(repository,sequence,actor_id,event) VALUES($1,$2,$3,$4)",
            &[repository, &integer(sequence)?, &auth.actor_id, &event],
        )?;
        tx.execute(
            "INSERT INTO alt_outbox(repository,sequence,event) VALUES($1,$2,$3)",
            &[repository, &integer(sequence)?, &event],
        )?;
        finish(
            &mut tx,
            &work.key,
            "committed",
            &result,
            unsigned(row.get("reserved_bytes"))?,
        )?;
        // SQL work and lock waits can outlive a lease checked on entry.
        crate::store::credential_row(&mut tx, &self.spec, credential)?;
        crate::tasks::recheck_current_authority(&mut tx, &self.spec, work, tasks, &current)?;
        crate::cloud_quota::check_publication(&mut tx, &self.spec)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(receipt)
    }
}
pub(crate) fn worker(tx: &mut Transaction<'_>, work: &PublicationWork) -> Result<postgres::Row> {
    let row = request_row(tx, &work.key)?.ok_or(Error::Precondition("request not registered"))?;
    let time = now(tx)?;
    active(&row, time)?;
    if unsigned(row.get("fence"))? != work.fence || unsigned(row.get("worker_lease_until"))? <= time
    {
        return Err(Error::Precondition("stale publication worker"));
    }
    Ok(row)
}
pub(crate) fn check_graph_pins(
    tx: &mut Transaction<'_>,
    work: &PublicationWork,
    graph: &VerifiedGraph,
) -> Result<()> {
    let repository = &work.key.scope.repository_instance_id;
    let pin_id = upload_pin(&work.key);
    let time = now(tx)?;
    for oid in graph.objects() {
        let (_, digest, bytes) = work
            .source
            .objects
            .get(&oid)
            .ok_or(Error::Integrity("graph outside authorized source"))?;
        let present=tx.query_opt("SELECT b.bytes FROM alt_blobs b JOIN alt_pins p ON p.repository=b.repository AND p.blake3=b.blake3 WHERE b.repository=$1 AND b.blake3=$2 AND b.state='stored' AND p.pin_id=$3 AND p.expires_at>$4",&[repository,digest,&pin_id,&integer(time)?])?.ok_or(Error::Precondition("object pin expired or fenced"))?;
        if unsigned(present.get(0))? != *bytes {
            return Err(Error::Integrity("pinned object size"));
        }
    }
    Ok(())
}
pub(crate) fn finish(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
    state: &str,
    result: &RequestState,
    reservation: u64,
) -> Result<()> {
    let repository = &key.scope.repository_instance_id;
    crate::cloud_quota::release(tx, key)?;
    tx.execute("UPDATE alt_requests SET state=$5,result=$6,reserved_bytes=0,worker_lease_until=0 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4",&[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&state,&serde_json::to_value(result)?])?;
    tx.execute(
        "UPDATE alt_repositories SET reserved_bytes=reserved_bytes-$2 WHERE id=$1",
        &[repository, &integer(reservation)?],
    )?;
    tx.execute("DELETE FROM alt_request_content WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?;
    tx.execute(
        "DELETE FROM alt_pins WHERE repository=$1 AND pin_id=$2",
        &[repository, &upload_pin(key)],
    )?;
    Ok(())
}
