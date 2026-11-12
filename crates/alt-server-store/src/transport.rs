use crate::Jsonb;
use crate::requests::{active, check_key, request_row, upload_request_row, upload_selection};
use crate::store::{authorize, credential_row, instance, integer, now, snapshot, unsigned};
use crate::{Credential, Error, InstanceSpec, PublicationWork, Result, Store};
use alt_remote_protocol::{ContentEntry, Manifest, Publication, RequestKey, Scope};

pub use alt_remote_protocol::{PrincipalInfo, PublishedBundle, RepositoryInfo, ReviewSelection};
impl Store {
    pub fn published_records(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        kind: alt_remote_protocol::RecordKind,
        after: Option<&str>,
        count: usize,
    ) -> Result<Vec<alt_remote_protocol::RecordRoot>> {
        if count == 0 || count > 100 {
            return Err(Error::Limit("record page size"));
        }
        if let Some(id) = after {
            alt_task::check_id(id).map_err(|_| Error::Unauthorized)?;
        }
        let name = alt_json::to_value(kind)?
            .as_str()
            .ok_or(Error::Integrity("record kind"))?
            .to_owned();
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        let records=tx.query("SELECT DISTINCT record_id FROM alt_task_records WHERE repository=$1 AND kind=$2 AND record_id>$3 ORDER BY record_id LIMIT $4", &[&scope.repository_instance_id,&name,&after.unwrap_or(""),&integer(count as u64)?])?.into_iter()
            .map(|row| alt_remote_protocol::RecordRoot {kind,id:row.get(0)}).collect();
        tx.commit()?;
        Ok(records)
    }
    /// A current reviewer inspects the exact selected pending publication.
    /// The response is received history plus present refs, not approval.
    pub fn review_selection(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<ReviewSelection> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (row, current) = review_request(&mut tx, &self.spec, credential, key)?;
        let (collector_generation, _) =
            crate::tasks::collector_policy(&mut tx, &key.scope.repository_instance_id)?;
        let result = ReviewSelection {
            publication: row.get::<_, Jsonb>("publication").read()?,
            manifest: row.get::<_, Jsonb>("manifest").read()?,
            current_refs: current
                .refs
                .into_iter()
                .map(|(name, oid)| (name, oid.to_string()))
                .collect(),
            policy_version: current.policy_version.to_string(),
            runtime_generation: self.spec.runtime_generation.to_string(),
            collector_generation: collector_generation.to_string(),
            revocation_generation: current.revocation_generation.to_string(),
        };
        tx.commit()?;
        Ok(result)
    }
    pub fn open_review_content(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<(u64, crate::StoredReader)> {
        alt_task::check_id(digest).map_err(|_| Error::Unauthorized)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (row, _) = review_request(&mut tx, &self.spec, credential, key)?;
        let manifest: Manifest = row.get::<_, Jsonb>("manifest").read()?;
        let entry = manifest
            .entries
            .iter()
            .find(|entry| entry.blake3 == digest)
            .ok_or(Error::Unauthorized)?;
        let time = integer(now(&mut tx)?)?;
        if tx.query_opt("SELECT 1 FROM alt_pins p JOIN alt_blobs b ON b.repository=p.repository AND b.blake3=p.blake3 WHERE p.repository=$1 AND p.pin_id=$2 AND p.blake3=$3 AND p.expires_at>$4 AND b.state='stored' AND b.bytes=$5", &[&key.scope.repository_instance_id,&crate::requests::upload_pin(key),&digest,&time,&integer(entry.bytes)?])?.is_none() {
            // a selected Git object that was published earlier was not uploaded again
            let entry = entry.clone();
            tx.commit()?;
            let segments = self.published_segments(&key.scope.repository_instance_id)?;
            if crate::pack_native::holds_entry(&segments, &entry)?
                && let Some(content) = segment_content(&segments, digest)?
            {
                return Ok(content);
            }
            return Err(Error::Precondition("review content pin expired"));
        }
        let location =
            crate::segments::location(&mut tx, &key.scope.repository_instance_id, digest)?;
        let file = self
            .bytes
            .read_payload(&key.scope, digest, entry.bytes, location.as_ref())?;
        tx.commit()?;
        Ok((entry.bytes, file))
    }
    /// Early admission for Git receive-pack before reading a potentially large
    /// body. Exact per-ref/path/force checks still occur at publication.
    pub fn check_git_receive_access(
        &mut self,
        credential: &Credential,
        repository: &str,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        // only row locks are taken; nothing here needs a synchronous flush
        tx.execute("SET LOCAL synchronous_commit=off", &[])?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if !current.writes_enabled
            || !auth
                .grants
                .iter()
                .any(|grant| grant.create || grant.update || grant.delete)
        {
            return Err(Error::Unauthorized);
        }
        tx.commit()?;
        Ok(())
    }
    pub fn receipt_public_key(&self) -> String {
        self.signing_key
            .verifying_key()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    pub fn principal(&mut self, credential: &Credential) -> Result<PrincipalInfo> {
        let mut tx = self.client.transaction()?;
        // only row locks are taken; nothing here needs a synchronous flush
        tx.execute("SET LOCAL synchronous_commit=off", &[])?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let row = credential_row(&mut tx, &self.spec, credential)?;
        let value = PrincipalInfo {
            actor_id: row.get("actor_id"),
            attribution: row.get::<_, Jsonb>("attribution").read()?,
        };
        tx.commit()?;
        Ok(value)
    }
    pub fn repository_info(
        &mut self,
        credential: &Credential,
        repository: &str,
    ) -> Result<RepositoryInfo> {
        let mut tx = self.client.transaction()?;
        // only row locks are taken; nothing here needs a synchronous flush
        tx.execute("SET LOCAL synchronous_commit=off", &[])?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        authorize(&mut tx, &self.spec, credential, &current)?;
        let result = RepositoryInfo {
            scope: current.scope,
            object_format: current.object_format,
            default_branch: tx
                .query_one(
                    "SELECT default_branch FROM alt_repositories WHERE id=$1",
                    &[&repository],
                )?
                .get(0),
            refs: current
                .refs
                .into_iter()
                .map(|(name, oid)| (name, oid.to_string()))
                .collect(),
            policy_version: current.policy_version.to_string(),
            revocation_generation: current.revocation_generation.to_string(),
        };
        tx.commit()?;
        alt_remote_protocol::validate_ref_name(&result.default_branch)?;
        if !result.default_branch.starts_with("refs/heads/") {
            return Err(Error::Integrity("repository default branch"));
        }
        Ok(result)
    }
    pub fn set_default_branch(
        &mut self,
        credential: &Credential,
        repository: &str,
        branch: &str,
    ) -> Result<()> {
        alt_remote_protocol::validate_ref_name(branch)?;
        if !branch.starts_with("refs/heads/") {
            return Err(Error::Precondition(
                "default branch must be under refs/heads/",
            ));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        snapshot(&mut tx, &self.spec, repository)?;
        crate::store::require_admin(&mut tx, &self.spec, credential)?;
        tx.execute(
            "UPDATE alt_repositories SET default_branch=$2 WHERE id=$1",
            &[&repository, &branch],
        )?;
        crate::browser::audit_admin(
            &mut tx,
            &self.spec,
            credential,
            "repository.default_branch",
            repository,
            Jsonb(alt_json::json!({"branch":branch})),
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
    /// Authenticate and select an upload before accepting or writing its body.
    /// Equal content can have multiple selected semantic roles; all entries are
    /// returned so registration verifies each of their chunk/mapping claims.
    pub fn selected_upload(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<Vec<ContentEntry>> {
        Ok(self.selected_upload_state(credential, key, digest)?.0)
    }
    /// the selected entries, and whether the request already has their content
    pub(crate) fn selected_upload_state(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<(Vec<ContentEntry>, bool)> {
        alt_task::check_id(digest).map_err(|_| Error::Unauthorized)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        check_key(key, &current.scope, &auth.actor_id)?;
        let row = upload_request_row(&mut tx, key)?.ok_or(Error::Unauthorized)?;
        active(&row, now(&mut tx)?)?;
        let publication: Publication = row.get::<_, Jsonb>("publication").read()?;
        alt_repository_service::authorize_intent(
            &publication,
            &current,
            &auth,
            self.limits,
            now(&mut tx)?,
        )?;
        let selection = upload_selection(&mut tx, key, digest)?.ok_or(Error::Unauthorized)?;
        if selection.entries.is_empty() {
            return Err(Error::Unauthorized);
        }
        tx.commit()?;
        Ok((selection.entries, selection.verified))
    }
    /// Relinquish only this fenced worker. It cannot extend a lease, change a
    /// request result or release another worker's newer incarnation.
    pub fn release_publication(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if current.scope != work.key.scope {
            return Err(Error::Unauthorized);
        }
        if auth.actor_id != work.key.actor_id {
            crate::tasks::reviewer(&mut tx, &self.spec, credential, &current)?;
        }
        crate::publication::worker(&mut tx, work)?;
        tx.execute("UPDATE alt_requests SET worker_lease_until=0 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 AND fence=$5", &[&work.key.scope.repository_instance_id,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id,&integer(work.fence)?])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
    /// Download a permanently published raw payload, never a digest guessed
    /// from another request's temporary upload. Open the descriptor under the
    /// same repository lock used by GC; later unlink cannot invalidate it.
    pub fn open_published_content(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
    ) -> Result<(u64, crate::StoredReader)> {
        alt_task::check_id(digest).map_err(|_| Error::Unauthorized)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        let Some(row) = tx.query_opt("SELECT b.bytes FROM alt_blobs b WHERE b.repository=$1 AND b.blake3=$2 AND b.state='stored' AND EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=b.repository AND p.blake3=b.blake3 AND p.expires_at IS NULL)", &[&scope.repository_instance_id,&digest])? else {
            tx.commit()?;
            return self.open_published_git_content(credential, scope, digest);
        };
        let size = unsigned(row.get(0))?;
        let location = crate::segments::location(&mut tx, &scope.repository_instance_id, digest)?;
        let file = self
            .bytes
            .read_payload(scope, digest, size, location.as_ref())?;
        tx.commit()?;
        Ok((size, file))
    }
    /// A published Git object by the digest of its content. Git objects live in the
    /// pack segments, whichever protocol brought them in.
    fn open_published_git_content(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
    ) -> Result<(u64, crate::StoredReader)> {
        let view = self.pack_view(credential, &scope.repository_instance_id)?;
        if &view.scope != scope {
            return Err(Error::Unauthorized);
        }
        segment_content(&view.segments, digest)?.ok_or(Error::Unauthorized)
    }
    /// Select published task bundles by any selected immutable record. Original manifest,
    /// publication and receipt are retained; a receiving client never invents
    /// a local request completion for the originating workspace.
    pub fn bundles_for_record(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        root: &alt_remote_protocol::RecordRoot,
        after_digest: Option<&str>,
        count: usize,
    ) -> Result<Vec<PublishedBundle>> {
        root.validate()?;
        if count == 0 || count > 100 {
            return Err(Error::Limit("bundle page size"));
        }
        if let Some(digest) = after_digest {
            alt_task::check_id(digest).map_err(|_| Error::Unauthorized)?;
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        let kind = alt_json::to_value(root.kind)?
            .as_str()
            .ok_or(Error::Integrity("task root kind"))?
            .to_owned();
        let rows = tx.query(concat!("SELECT publication,manifest,receipt FROM (SELECT q.digest,q.publication,q.manifest,", crate::committed::receipt_sql!(), " AS receipt FROM alt_task_bundle_records r JOIN alt_requests q ON q.repository=r.repository AND q.epoch=r.epoch AND q.actor_id=r.actor_id AND q.request_id=r.request_id WHERE r.repository=$1 AND r.kind=$2 AND r.record_id=$3 AND q.state='committed' UNION ALL SELECT p.digest,p.publication,p.manifest,p.receipt FROM alt_archived_records r JOIN alt_archived_publications p ON p.repository=r.repository AND p.digest=r.digest WHERE r.repository=$1 AND r.kind=$2 AND r.record_id=$3) h WHERE digest>$4 ORDER BY digest LIMIT $5"), &[&scope.repository_instance_id,&kind,&root.id,&after_digest.unwrap_or(""),&integer(count as u64)?])?;
        let mut result = Vec::new();
        let mut bytes = 0usize;
        for row in rows {
            let publication: Jsonb = row.get(0);
            let manifest: Jsonb = row.get(1);
            let receipt: alt_remote_protocol::Receipt = row.get::<_, Jsonb>(2).read()?;
            let publication = publication.read()?;
            let provenance = crate::provenance::load(
                &mut tx,
                scope,
                &publication,
                &receipt,
                &self.signing_key,
                self.limits,
            )?;
            let bundle = PublishedBundle {
                publication,
                manifest: manifest.read()?,
                receipt,
                provenance,
            };
            bytes = bytes
                .checked_add(alt_json::to_vec(&bundle)?.len())
                .ok_or(Error::Limit("bundle page bytes"))?;
            if bytes > self.limits.total_bytes.min(usize::MAX as u64) as usize {
                return Err(Error::Limit("bundle page bytes"));
            }
            result.push(bundle);
        }
        tx.commit()?;
        Ok(result)
    }
}
fn review_request(
    tx: &mut crate::database::Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
    key: &RequestKey,
) -> Result<(alt_pg::Row, alt_repository_service::RepositorySnapshot)> {
    key.validate()?;
    let current = snapshot(tx, spec, &key.scope.repository_instance_id)?;
    crate::tasks::reviewer(tx, spec, credential, &current)?;
    if current.scope != key.scope {
        return Err(Error::Unauthorized);
    }
    let row = request_row(tx, key)?.ok_or(Error::Unauthorized)?;
    active(&row, now(tx)?)?;
    if row.get::<_, String>("state") != "ready" {
        return Err(Error::Precondition("publication not ready for review"));
    }
    Ok((row, current))
}

/// The object whose content has this digest, inflated into an unnamed temporary file
/// as it is read from its segment.
fn segment_content(
    segments: &alt_pack_segment::SegmentSet,
    digest: &str,
) -> Result<Option<(u64, crate::StoredReader)>> {
    let key = alt_provenance::decode_hex::<32>(digest).map_err(|_| Error::Unauthorized)?;
    let Some((_, _, size, mut stream)) = segments
        .open_blake3(&key)
        .map_err(|_| Error::Integrity("published object bytes"))?
    else {
        return Ok(None);
    };
    let mut file = alt_os::temp::tempfile()?;
    if std::io::copy(&mut stream, &mut file)? != size {
        return Err(Error::Integrity("published object bytes"));
    }
    Ok(Some((size, crate::StoredReader::from_file(file, size)?)))
}
