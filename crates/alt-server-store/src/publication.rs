mod refs;

use crate::database::Transaction;
use crate::requests::{active, check_key, pin_many, request_row, upload_pin};
use crate::store::{authorize, instance, integer, now, snapshot, unsigned};
use crate::{ByteStore, Credential, Error, Result, Store};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_remote_protocol::{Manifest, Publication, Receipt, ReceiptBody, RequestKey, RequestState};
use alt_repository_service::{ObjectReader, ObjectSource, VerifiedGraph, validate_publication};
use ed25519_dalek::Signer;
use std::collections::{BTreeMap, BTreeSet};

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
    /// Finish physical index verification before exposing a buffered review.
    /// This grants no authority; callers still need current store checks.
    pub fn check_read_integrity(&self) -> Result<()> {
        self.source.bytes.finish()
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
    bytes: crate::bytes::ReadSession,
    pub(crate) objects: BTreeMap<ObjectId, (ObjectKind, String, u64)>,
    artifacts: BTreeMap<String, u64>,
    locations: crate::segments::Locations,
    /// Git objects published earlier, whichever protocol brought them in
    pub(crate) segments: alt_pack_segment::SegmentSet,
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
            .read(
                &artifact.blake3,
                artifact.bytes,
                self.locations.get(&artifact.blake3),
            )
            .map_err(|_| alt_task_service::Error::Integrity("selected artifact unavailable"))?;
        Ok(Box::new(file))
    }
}
impl ObjectSource for PinnedSource {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        let Some((kind, digest, bytes)) = self.objects.get(&oid) else {
            let Ok(key) = <[u8; 20]>::try_from(oid.as_bytes()) else {
                return Ok(None);
            };
            return match self.segments.read(&key) {
                Ok((kind, data)) => Ok(Some(ObjectReader {
                    kind: crate::packs::object_kind(kind),
                    bytes: data.len() as u64,
                    reader: Box::new(std::io::Cursor::new(data)),
                })),
                Err(alt_pack_segment::Error::NotFound(_)) => Ok(None),
                Err(_) => Err(alt_repository_service::Error::Integrity(
                    "pack segment object",
                )),
            };
        };
        let file = self
            .bytes
            .read(digest, *bytes, self.locations.get(digest))
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
        for batch in manifest.entries.chunks(crate::database::BATCH_ROWS) {
            let digests: Vec<_> = batch.iter().map(|entry| entry.blake3.as_str()).collect();
            let content: BTreeMap<String, postgres::Row> = tx.query("SELECT c.blake3,c.verified,b.bytes,b.state FROM alt_request_content c LEFT JOIN alt_blobs b ON b.repository=c.repository AND b.blake3=c.blake3 WHERE c.repository=$1 AND c.epoch=$2 AND c.actor_id=$3 AND c.request_id=$4 AND c.blake3=ANY($5)", &[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&digests])?
                .into_iter().map(|row| (row.get(0), row)).collect();
            let oids: Vec<_> = batch
                .iter()
                .filter_map(|entry| entry.git_oid.as_deref())
                .collect();
            let mappings: BTreeMap<String, (String, String)> = tx
                .query(
                    "SELECT oid,kind,blake3 FROM alt_objects WHERE repository=$1 AND oid=ANY($2)",
                    &[repository, &oids],
                )?
                .into_iter()
                .map(|row| (row.get(0), (row.get(1), row.get(2))))
                .collect();
            for entry in batch {
                let row = content
                    .get(&entry.blake3)
                    .filter(|row| row.get::<_, bool>(1))
                    .ok_or(Error::Precondition(
                        "selected content not uploaded or published",
                    ))?;
                if let Some(oid) = &entry.git_oid {
                    let (actual_kind, actual_digest) = mappings
                        .get(oid)
                        .ok_or(Error::Precondition("Git mapping not uploaded"))?;
                    let kind = match entry.kind {
                        alt_remote_protocol::ContentKind::Blob => "blob",
                        alt_remote_protocol::ContentKind::Tree => "tree",
                        alt_remote_protocol::ContentKind::Commit => "commit",
                        alt_remote_protocol::ContentKind::Tag => "tag",
                        _ => return Err(Error::Integrity("Git mapping kind")),
                    };
                    if actual_kind != kind || actual_digest != &entry.blake3 {
                        return Err(Error::Integrity("Git mapping selection"));
                    }
                }
                let bytes = row
                    .get::<_, Option<i64>>(2)
                    .ok_or(Error::Precondition("upload incomplete"))?;
                if unsigned(bytes)? != entry.bytes
                    || row.get::<_, Option<String>>(3).as_deref() != Some("stored")
                {
                    return Err(Error::Precondition("upload incomplete"));
                }
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
            objects.insert(oid, (kind, digest, bytes));
        }
        let digests: Vec<_> = objects
            .values()
            .map(|(_, digest, _)| digest.as_str())
            .collect();
        pin_many(&mut tx, repository, &pin_id, &digests, Some(expires))?;
        let fence=unsigned(tx.query_one("UPDATE alt_requests SET fence=fence+1,worker_lease_until=$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 RETURNING fence",&[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&integer(lease_until)?])?.get(0))?;
        let artifacts: BTreeMap<String, u64> = manifest
            .entries
            .iter()
            .map(|e| (e.blake3.clone(), e.bytes))
            .collect();
        let all_digests: Vec<_> = objects
            .values()
            .map(|(_, digest, _)| digest.as_str())
            .chain(artifacts.keys().map(String::as_str))
            .collect();
        let locations = crate::segments::locations(&mut tx, repository, &all_digests)?;
        let segment_ids = Self::live_segment_ids(&mut tx, repository)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        let segments = self.open_segments(&key.scope.repository_instance_id, &segment_ids)?;
        Ok(PublicationWork {
            key: key.clone(),
            fence,
            publication,
            manifest,
            source: PinnedSource {
                bytes: crate::bytes::ReadSession::new(&self.bytes, &key.scope),
                objects,
                artifacts,
                locations,
                segments,
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
            let reader = work.source.bytes.read(
                &entry.blake3,
                entry.bytes,
                work.source.locations.get(&entry.blake3),
            )?;
            ByteStore::verify_chunk_reader(reader, entry)?;
        }
        work.source.bytes.finish()?;
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
        crate::telemetry::request(&work.key);
        crate::telemetry::observe(alt_server_telemetry::Phase::Commit, || {
            let receipt = self.publish_checked_inner(credential, work, graph, tasks)?;
            crate::telemetry::committed(&receipt);
            Ok(receipt)
        })
    }
    fn publish_checked_inner(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: Option<&alt_task_service::VerifiedTaskGraph>,
    ) -> Result<Receipt> {
        // Recheck before taking SQL locks. A durable committed receipt remains
        // replayable even if its formerly selected byte files are unavailable.
        let read_integrity = work.check_read_integrity();
        // a failure here must not hide the receipt of an already committed request
        let (materialized, unavailable) = match self.materialize_publication(work, graph) {
            Ok(segment) => (segment, false),
            Err(_) => (None, true),
        };
        let result = self.publish_transaction(
            credential,
            work,
            graph,
            tasks,
            read_integrity,
            // None: the request's Git objects could not be read back
            (!unavailable).then_some(materialized.as_ref()),
        );
        match (&result, materialized) {
            (Ok(_), Some(segment)) => self.adopt_segment(
                &work.key.scope.repository_instance_id,
                &segment.id,
                segment.segment,
            )?,
            (Err(_), Some(segment)) => {
                // nothing references an unpublished segment
                let _ = std::fs::remove_file(&segment.path);
            }
            _ => {}
        }
        result
    }
    /// the request's Git objects that no segment holds yet, packed into a new segment
    fn materialize_publication(
        &mut self,
        work: &PublicationWork,
        graph: &VerifiedGraph,
    ) -> Result<Option<crate::packs::Materialized>> {
        use alt_repository_service::ObjectSource;
        let mut objects = Vec::new();
        for oid in graph.objects() {
            let Ok(key) = <[u8; 20]>::try_from(oid.as_bytes()) else {
                continue;
            };
            if !work.source.objects.contains_key(&oid) || work.source.segments.locate_any(&key) {
                continue;
            }
            let mut object = work
                .source
                .open(oid)?
                .ok_or(Error::Integrity("selected Git object unavailable"))?;
            let mut data = Vec::with_capacity(object.bytes as usize);
            std::io::Read::read_to_end(&mut object.reader, &mut data)?;
            let kind = match object.kind {
                ObjectKind::Commit => alt_pack_segment::Kind::Commit,
                ObjectKind::Tree => alt_pack_segment::Kind::Tree,
                ObjectKind::Blob => alt_pack_segment::Kind::Blob,
                ObjectKind::Tag => alt_pack_segment::Kind::Tag,
            };
            objects.push((kind, data));
        }
        let existing = work.source.segments.clone();
        self.materialize(&work.key.scope.repository_instance_id, &existing, objects)
    }
    fn publish_transaction(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: Option<&alt_task_service::VerifiedTaskGraph>,
        read_integrity: Result<()>,
        materialized: Option<Option<&crate::packs::Materialized>>,
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
        read_integrity?;
        let Some(materialized) = materialized else {
            return Err(Error::Integrity("selected Git object unavailable"));
        };
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
        refs::replace(&mut tx, work, graph, &validated.updates)?;
        let unreachable = work
            .publication
            .refs
            .iter()
            .any(|update| update.force || update.new.is_none());
        let sequence=unsigned(tx.query_one("UPDATE alt_repositories SET transaction_sequence=transaction_sequence+1,pack_unreachable=pack_unreachable OR $2 WHERE id=$1 RETURNING transaction_sequence",&[repository,&unreachable])?.get(0))?;
        if let Some(segment) = materialized {
            crate::packs::insert_segment(
                &mut tx,
                repository,
                sequence,
                &crate::NewSegment {
                    id: &segment.id,
                    objects: segment.objects,
                    bytes: segment.bytes,
                    flags: segment.flags,
                    inline: segment.inline.as_deref(),
                },
            )?;
        }
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
            signing_key_id: alt_hash::blake3::hash(self.signing_key.verifying_key().as_bytes())
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
    let objects: Vec<_> = graph.objects().collect();
    for batch in objects.chunks(crate::database::BATCH_ROWS) {
        // objects held by a segment are protected by the repository's closure, not by pins
        let mut selected = Vec::new();
        for oid in batch {
            match work.source.objects.get(oid) {
                Some(found) => selected.push(found),
                None if <[u8; 20]>::try_from(oid.as_bytes())
                    .is_ok_and(|key| work.source.segments.locate_any(&key)) => {}
                None => return Err(Error::Integrity("graph outside authorized source")),
            }
        }
        let digests: Vec<_> = selected
            .iter()
            .map(|(_, digest, _)| digest.as_str())
            .collect();
        // Keep each lookup bounded independently. A join planned against a
        // freshly populated catalog can repeatedly scan this request's entire
        // pin set for each digest. Both observations use the same repository
        // lock and transaction as the caller's current publication checks.
        let present: BTreeMap<String, i64> = tx.query("SELECT blake3,bytes FROM alt_blobs WHERE repository=$1 AND blake3=ANY($2) AND state='stored'", &[repository,&digests])?
            .into_iter().map(|row| (row.get(0),row.get(1))).collect();
        let pinned: BTreeSet<String> = tx.query("SELECT blake3 FROM alt_pins WHERE repository=$1 AND pin_id=$2 AND blake3=ANY($3) AND expires_at>$4", &[repository,&pin_id,&digests,&integer(time)?])?
            .into_iter().map(|row| row.get(0)).collect();
        for (_, digest, bytes) in selected {
            let actual = present
                .get(digest)
                .filter(|_| pinned.contains(digest))
                .ok_or(Error::Precondition("object pin expired or fenced"))?;
            if unsigned(*actual)? != *bytes {
                return Err(Error::Integrity("pinned object size"));
            }
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
