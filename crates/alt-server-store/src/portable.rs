//! Logical, authenticated snapshots. Physical disaster backups stay separate.
use crate::store::{authorize, instance, integer, now, require_admin, snapshot, unsigned};
use crate::{ByteStore, Credential, Error, Result, Store};
use alt_portable::{
    Blob, BlobSource, Catalog, Envelope, HistoricalPublication, IdentityRequirement, Object,
    Reference, VerificationLimits, VerifiedSnapshot,
};
use alt_remote_protocol::{ContentKind, Publication, RequestKey, RequestState, Scope};
use postgres::Transaction;

use std::{collections::BTreeMap, io::Read, os::unix::fs::MetadataExt};

pub use alt_portable::http::ExportState as PortableExportState;
type Stamp = (u64, u64, u64, i64, i64, i64, i64);

/// An authorized, pinned snapshot. Possessing its ID alone never permits a new
/// download: obtaining another handle rechecks the current account and scope.
pub struct PortableExport {
    envelope: Envelope,
    documents: BTreeMap<String, Vec<u8>>,
    bytes: ByteStore,
}
/// Completed byte validation, bound to immutable intent and byte-store inodes.
/// This grants no authority; completion rechecks current permissions and epoch.
pub struct PortableExportWork {
    export: PortableExport,
    stamps: Vec<Stamp>,
    limits: VerificationLimits,
}
impl PortableExport {
    pub fn envelope(&self) -> &Envelope {
        &self.envelope
    }
    fn stamps(&self, bytes: &ByteStore) -> Result<Vec<Stamp>> {
        self.envelope
            .catalog
            .blobs
            .iter()
            .filter(|b| !self.documents.contains_key(&b.blake3))
            .map(|b| {
                let m = bytes
                    .read(&self.envelope.catalog.source, &b.blake3, b.bytes)?
                    .metadata()?;
                Ok((
                    m.dev(),
                    m.ino(),
                    m.len(),
                    m.ctime(),
                    m.ctime_nsec(),
                    m.mtime(),
                    m.mtime_nsec(),
                ))
            })
            .collect()
    }
}
impl BlobSource for PortableExport {
    fn open(&self, blob: &Blob) -> alt_portable::Result<Box<dyn Read + '_>> {
        if self
            .envelope
            .catalog
            .blobs
            .binary_search_by(|b| b.blake3.cmp(&blob.blake3))
            .ok()
            .is_none_or(|i| self.envelope.catalog.blobs[i] != *blob)
        {
            return Err(alt_portable::Error::Invalid(
                "export payload outside snapshot",
            ));
        }
        if let Some(document) = self.documents.get(&blob.blake3) {
            return Ok(Box::new(std::io::Cursor::new(document)));
        }
        Ok(Box::new(
            self.bytes
                .read(&self.envelope.catalog.source, &blob.blake3, blob.bytes)
                .map_err(|e| std::io::Error::other(e.to_string()))?,
        ))
    }
}

fn export_key(scope: &Scope, actor: &str, id: &str) -> Result<()> {
    RequestKey {
        scope: scope.clone(),
        actor_id: actor.into(),
        request_id: id.into(),
    }
    .validate()?;
    Ok(())
}
fn pin_id(id: &str) -> String {
    format!("portable-export:{id}")
}
fn identity(installation: &str, actor: &str) -> IdentityRequirement {
    IdentityRequirement {
        installation_id: installation.into(),
        actor_id: actor.into(),
        had_read: false,
        had_publish_records: false,
        had_review: false,
        ref_grants: vec![],
    }
}
fn document(
    documents: &mut BTreeMap<String, Vec<u8>>,
    blobs: &mut BTreeMap<String, Blob>,
    value: &impl serde::Serialize,
    budget: &mut usize,
) -> Result<Blob> {
    let raw = serde_json::to_vec(value)?;
    let blob = Blob::of(&raw);
    if !documents.contains_key(&blob.blake3) {
        *budget = budget
            .checked_sub(raw.len())
            .ok_or(Error::Limit("portable logical metadata"))?;
        documents.insert(blob.blake3.clone(), raw);
    }
    if let Some(old) = blobs.insert(blob.blake3.clone(), blob.clone())
        && old != blob
    {
        return Err(Error::Integrity("portable metadata identity collision"));
    }
    Ok(blob)
}
fn object_kind(kind: &str) -> Result<ContentKind> {
    Ok(match kind {
        "blob" => ContentKind::Blob,
        "tree" => ContentKind::Tree,
        "commit" => ContentKind::Commit,
        "tag" => ContentKind::Tag,
        _ => return Err(Error::Integrity("portable Git kind")),
    })
}
fn audit(
    tx: &mut Transaction<'_>,
    actor: &str,
    action: &str,
    scope: &Scope,
    id: &str,
) -> Result<()> {
    crate::browser::audit_details(
        tx,
        actor,
        action,
        &scope.repository_instance_id,
        &serde_json::json!({"export_id":id,"scope":scope}),
    )
}

impl Store {
    /// Capture one immutable watermark and its retention roots, validate bytes
    /// outside SQL locks, then recheck current authority before exposing it.
    /// A failed/interrupted validation remains a private, resumable intent.
    pub fn prepare_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: VerificationLimits,
    ) -> Result<PortableExport> {
        let work = self.verify_portable_export(credential, scope, id, limits)?;
        self.complete_portable_export(credential, work)
    }
    /// Validate outside SQL locks. A transport may refresh its platform lease
    /// after this phase; the store never extends authority on its own.
    pub fn verify_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: VerificationLimits,
    ) -> Result<PortableExportWork> {
        let (result, ready) = self.capture_portable_export(credential, scope, id, limits)?;
        let before = result.stamps(&self.bytes)?;
        if !ready {
            VerifiedSnapshot::verify(
                &result.envelope,
                scope,
                self.signing_key.verifying_key().as_bytes(),
                &result,
                limits,
            )?;
        }
        if before != result.stamps(&self.bytes)? {
            return Err(Error::Precondition(
                "portable export bytes changed during validation",
            ));
        }
        Ok(PortableExportWork {
            export: result,
            stamps: before,
            limits,
        })
    }
    /// Expose only the original verified snapshot under current authority.
    pub fn complete_portable_export(
        &mut self,
        credential: &Credential,
        work: PortableExportWork,
    ) -> Result<PortableExport> {
        let PortableExportWork {
            mut export,
            stamps,
            limits,
        } = work;
        if stamps != export.stamps(&self.bytes)? {
            return Err(Error::Precondition(
                "portable verified export changed or belongs to another byte store",
            ));
        }
        export.bytes = self.bytes.clone();
        let result = export;
        let scope = &result.envelope.catalog.source;
        let id = result.envelope.catalog.export_id.as_str();
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if current.scope != *scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let repository = &scope.repository_instance_id;
        let row = tx.query_opt("SELECT actor_id,envelope,retired,verified_at FROM alt_portable_exports WHERE repository=$1 AND export_id=$2 FOR UPDATE", &[repository,&id])?
            .ok_or(Error::Precondition("portable export intent missing"))?;
        let recorded: Envelope = serde_json::from_value(row.get(1))?;
        if row.get::<_, String>(0) != auth.actor_id
            || row.get::<_, bool>(2)
            || recorded.encode(limits.package)? != result.envelope.encode(limits.package)?
        {
            return Err(Error::Precondition(
                "portable export intent changed or retired",
            ));
        }
        // All ordinary byte-store writers are immutable. GC must first take
        // the repository barrier and cannot delete these retained roots. The
        // filesystem stamps above therefore require no long transaction.
        let rows = tx.query("SELECT b.blake3,b.bytes FROM alt_blobs b JOIN alt_pins p ON p.repository=b.repository AND p.blake3=b.blake3 WHERE b.repository=$1 AND p.pin_id=$2 AND p.expires_at IS NULL AND b.state='stored' ORDER BY b.blake3", &[repository,&pin_id(id)])?;
        let actual = rows
            .iter()
            .map(|r| {
                Ok(Blob {
                    blake3: r.get(0),
                    bytes: unsigned(r.get(1))?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let retained: std::collections::BTreeSet<_> =
            actual.iter().map(|b| b.blake3.as_str()).collect();
        // A logical metadata document can deduplicate an already retained raw
        // blob. Permit that pin, while requiring every nonvirtual payload.
        let expected: Vec<_> = result
            .envelope
            .catalog
            .blobs
            .iter()
            .filter(|b| {
                !result.documents.contains_key(&b.blake3) || retained.contains(b.blake3.as_str())
            })
            .cloned()
            .collect();
        if actual != expected {
            return Err(Error::Precondition("portable export retention changed"));
        }
        require_admin(&mut tx, &self.spec, credential)?;
        authorize(&mut tx, &self.spec, credential, &current)?;
        if row.get::<_, Option<i64>>(3).is_none() {
            let verified_at = integer(now(&mut tx)?)?;
            tx.execute("UPDATE alt_portable_exports SET verified_at=$3 WHERE repository=$1 AND export_id=$2", &[repository,&id,&verified_at])?;
            audit(
                &mut tx,
                &auth.actor_id,
                "portable.export.completed",
                scope,
                id,
            )?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
    /// Persist the original metadata and retention in one short transaction.
    fn capture_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: VerificationLimits,
    ) -> Result<(PortableExport, bool)> {
        limits.package.validate()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let repository = &scope.repository_instance_id;
        if let Some(row) = tx.query_opt(
            "SELECT actor_id,envelope,documents,retired,verified_at FROM alt_portable_exports WHERE repository=$1 AND export_id=$2",
            &[repository, &id],
        )? {
            if row.get::<_, String>(0) != auth.actor_id || row.get::<_, bool>(3) {
                return Err(Error::Precondition("export belongs to another actor or was retired"));
            }
            let envelope: Envelope = serde_json::from_value(row.get(1))?;
            envelope.verify(scope, self.signing_key.verifying_key().as_bytes(), limits.package)?;
            let result = PortableExport {
                envelope,
                documents: serde_json::from_value(row.get(2))?,
                bytes: self.bytes.clone(),
            };
            tx.commit()?;
            return Ok((result, row.get::<_,Option<i64>>(4).is_some()));
        }
        let active: i64 = tx
            .query_one(
                "SELECT count(*) FROM alt_portable_exports WHERE repository=$1 AND NOT retired",
                &[repository],
            )?
            .get(0);
        if active >= 16 {
            return Err(Error::Limit(
                "retire an old portable export before creating another",
            ));
        }
        let metadata = tx.query_one(
            "SELECT transaction_sequence,default_branch FROM alt_repositories WHERE id=$1",
            &[repository],
        )?;
        let mut catalog = Catalog {
            version: alt_portable::VERSION,
            source: scope.clone(),
            source_mode: self.spec.deployment_mode,
            source_public_key: alt_provenance::encode_hex(
                self.signing_key.verifying_key().as_bytes(),
            ),
            export_id: id.into(),
            snapshot_sequence: unsigned(metadata.get(0))?,
            object_format: current.object_format,
            default_branch: metadata.get(1),
            protections: current.policies.clone(),
            refs: current
                .refs
                .iter()
                .map(|(name, oid)| Reference {
                    name: name.clone(),
                    oid: oid.to_string(),
                })
                .collect(),
            objects: vec![],
            publications: vec![],
            import_receipts: vec![],
            change_links: vec![],
            identities: vec![],
            blobs: vec![],
        };
        let bound = integer((limits.package.entries as u64).saturating_add(1))?;
        catalog.change_links = crate::link_history::list(
            &mut tx,
            repository,
            limits.package.entries,
            limits.package.metadata_bytes,
        )?;
        // Do not recursively include previous export snapshots. Only live
        // publication roots, including retained task history, seed this one.
        let rows = tx.query("SELECT b.blake3,b.bytes,b.state FROM alt_blobs b WHERE b.repository=$1 AND EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=b.repository AND p.blake3=b.blake3 AND p.expires_at IS NULL AND p.pin_id NOT LIKE 'portable-export:%') ORDER BY b.blake3 LIMIT $2", &[repository,&bound])?;
        if rows.len() > limits.package.entries {
            return Err(Error::Limit("portable payload inventory"));
        }
        let mut blobs = BTreeMap::new();
        for row in rows {
            if row.get::<_, String>(2) != "stored" {
                return Err(Error::Integrity("portable published content missing"));
            }
            let blob = Blob {
                blake3: row.get(0),
                bytes: unsigned(row.get(1))?,
            };
            crate::requests::pin(&mut tx, repository, &pin_id(id), &blob.blake3, None)?;
            blobs.insert(blob.blake3.clone(), blob);
        }
        let rows = tx.query("SELECT o.oid,o.kind,o.blake3 FROM alt_objects o JOIN alt_pins p ON p.repository=o.repository AND p.blake3=o.blake3 WHERE o.repository=$1 AND p.pin_id=$2 ORDER BY o.oid LIMIT $3", &[repository,&pin_id(id),&bound])?;
        if rows.len() > limits.package.entries {
            return Err(Error::Limit("portable Git inventory"));
        }
        for row in rows {
            let digest: String = row.get(2);
            catalog.objects.push(Object {
                oid: row.get(0),
                kind: object_kind(&row.get::<_, String>(1))?,
                payload: blobs
                    .get(&digest)
                    .ok_or(Error::Integrity("portable object payload"))?
                    .clone(),
            });
        }
        let rows = tx.query("SELECT a.actor_id,COALESCE(g.can_read,false),COALESCE(g.publish_records,false),COALESCE(g.grants,'[]'::jsonb),COALESCE(r.can_review,false) FROM (SELECT actor_id FROM alt_grants WHERE repository=$1 UNION SELECT actor_id FROM alt_review_grants WHERE repository=$1) a LEFT JOIN alt_grants g ON g.repository=$1 AND g.actor_id=a.actor_id LEFT JOIN alt_review_grants r ON r.repository=$1 AND r.actor_id=a.actor_id ORDER BY a.actor_id LIMIT $2", &[repository,&bound])?;
        if rows.len() > limits.package.entries {
            return Err(Error::Limit("portable identity inventory"));
        }
        let mut identities = BTreeMap::new();
        for row in rows {
            let actor: String = row.get(0);
            identities.insert(
                (scope.installation_id.clone(), actor.clone()),
                IdentityRequirement {
                    installation_id: scope.installation_id.clone(),
                    actor_id: actor,
                    had_read: row.get(1),
                    had_publish_records: row.get(2),
                    had_review: row.get(4),
                    ref_grants: serde_json::from_value(row.get(3))?,
                },
            );
        }
        let bound = integer((limits.package.publications as u64).saturating_add(1))?;
        let rows = tx.query("SELECT q.publication,q.manifest,q.result FROM alt_requests q JOIN alt_task_bundles b ON b.repository=q.repository AND b.epoch=q.epoch AND b.actor_id=q.actor_id AND b.request_id=q.request_id WHERE q.repository=$1 AND q.state='committed' ORDER BY q.digest LIMIT $2", &[repository,&bound])?;
        if rows.len() > limits.package.publications {
            return Err(Error::Limit("portable task publication inventory"));
        }
        let mut documents = BTreeMap::new();
        let mut budget = limits.package.metadata_bytes;
        for row in rows {
            let publication: Publication = serde_json::from_value(row.get(0))?;
            let manifest: alt_remote_protocol::Manifest = serde_json::from_value(row.get(1))?;
            let RequestState::Committed { receipt } = serde_json::from_value(row.get(2))? else {
                return Err(Error::Integrity("portable original receipt"));
            };
            let proofs = crate::provenance::load(
                &mut tx,
                scope,
                &publication,
                &receipt,
                &self.signing_key,
                self.limits,
            )?;
            let origin = &publication.key.scope.installation_id;
            for actor in std::iter::once(&publication.key.actor_id)
                .chain(publication.attribution.on_behalf_of.iter())
            {
                identities
                    .entry((origin.clone(), actor.clone()))
                    .or_insert_with(|| identity(origin, actor));
            }
            catalog.publications.push(HistoricalPublication {
                digest: publication.digest(self.limits)?,
                publication: document(&mut documents, &mut blobs, &publication, &mut budget)?,
                manifest: document(&mut documents, &mut blobs, &manifest, &mut budget)?,
                receipt: document(&mut documents, &mut blobs, &receipt, &mut budget)?,
                proofs,
            });
        }
        let bound = integer((limits.package.publications as u64).saturating_add(1))?;
        let rows = tx.query("SELECT publication,receipt,original FROM alt_archived_publications WHERE repository=$1 ORDER BY digest LIMIT $2", &[repository,&bound])?;
        if rows.len().saturating_add(catalog.publications.len()) > limits.package.publications {
            return Err(Error::Limit("portable historical publication inventory"));
        }
        for row in rows {
            let publication: Publication = serde_json::from_value(row.get(0))?;
            let receipt: alt_remote_protocol::Receipt = serde_json::from_value(row.get(1))?;
            let mut original: HistoricalPublication = serde_json::from_value(row.get(2))?;
            original.proofs = crate::provenance::load(
                &mut tx,
                scope,
                &publication,
                &receipt,
                &self.signing_key,
                self.limits,
            )?;
            // Original metadata payload descriptors remain unchanged, including
            // insignificant JSON whitespace in an imported signed document.
            catalog.publications.push(original);
        }
        catalog.publications.sort_by(|a, b| a.digest.cmp(&b.digest));
        let bound = integer((limits.package.entries as u64).saturating_add(1))?;
        let rows = tx.query("SELECT envelope,receipt FROM alt_portable_imports WHERE repository=$1 AND state='committed' ORDER BY import_id LIMIT $2", &[repository,&bound])?;
        if rows.len() > limits.package.entries {
            return Err(Error::Limit("portable import history inventory"));
        }
        let mut receipts = BTreeMap::new();
        for row in rows {
            let original: Envelope = serde_json::from_value(row.get(0))?;
            let receipt: alt_portable::ImportReceipt = serde_json::from_value(row.get(1))?;
            for item in original.catalog.identities {
                let key = (item.installation_id.clone(), item.actor_id.clone());
                if let Some(existing) = identities.get_mut(&key) {
                    // These are accumulated historical requirements, never
                    // installed capabilities. Return migrations can encounter
                    // the same qualified identity with changed permissions.
                    existing.had_read |= item.had_read;
                    existing.had_publish_records |= item.had_publish_records;
                    existing.had_review |= item.had_review;
                    let mut grants = BTreeMap::new();
                    for grant in existing.ref_grants.iter().chain(&item.ref_grants) {
                        grants.insert(serde_json::to_string(grant)?, grant.clone());
                    }
                    existing.ref_grants = grants.into_values().collect();
                } else {
                    identities.insert(key, item);
                }
            }
            let target = &receipt.body.intent.key.scope.installation_id;
            for actor in std::iter::once(&receipt.body.intent.key.actor_id).chain(
                receipt
                    .body
                    .intent
                    .mappings
                    .iter()
                    .filter_map(|m| m.target_actor.as_ref()),
            ) {
                identities
                    .entry((target.clone(), actor.clone()))
                    .or_insert_with(|| identity(target, actor));
            }
            for blob in original.catalog.import_receipts {
                receipts.insert(blob.blake3.clone(), blob);
            }
            let blob = document(&mut documents, &mut blobs, &receipt, &mut budget)?;
            receipts.insert(blob.blake3.clone(), blob);
        }
        catalog.import_receipts = receipts.into_values().collect();
        catalog.blobs = blobs.into_values().collect();
        catalog.identities = identities.into_values().collect();
        let result = PortableExport {
            envelope: Envelope::sign(catalog, &self.signing_key, limits.package)?,
            documents,
            bytes: self.bytes.clone(),
        };
        // Capturing metadata cannot extend an expired authority lease.
        require_admin(&mut tx, &self.spec, credential)?;
        authorize(&mut tx, &self.spec, credential, &current)?;
        let created_at = integer(now(&mut tx)?)?;
        tx.execute("INSERT INTO alt_portable_exports(repository,export_id,actor_id,envelope,documents,created_at) VALUES($1,$2,$3,$4,$5,$6)", &[repository,&id,&auth.actor_id,&serde_json::to_value(&result.envelope)?,&serde_json::to_value(&result.documents)?,&created_at])?;
        audit(
            &mut tx,
            &auth.actor_id,
            "portable.export.preparing",
            scope,
            id,
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok((result, false))
    }

    /// Resume an existing immutable export. This read never silently captures
    /// a newer snapshot, nor does an old ID bypass current account authority.
    pub fn open_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: alt_portable::PackageLimits,
    ) -> Result<PortableExport> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let row = tx.query_opt("SELECT envelope,documents FROM alt_portable_exports WHERE repository=$1 AND export_id=$2 AND NOT retired AND verified_at IS NOT NULL", &[&scope.repository_instance_id,&id])?.ok_or(Error::Precondition("active portable export not found"))?;
        let envelope: Envelope = serde_json::from_value(row.get(0))?;
        envelope.verify(scope, self.signing_key.verifying_key().as_bytes(), limits)?;
        let result = PortableExport {
            envelope,
            documents: serde_json::from_value(row.get(1))?,
            bytes: self.bytes.clone(),
        };
        tx.commit()?;
        Ok(result)
    }

    /// The source remains present. Only an unchanged snapshot can become the
    /// cutover watermark, atomically blocking both new and in-flight writes.
    pub fn freeze_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: alt_portable::PackageLimits,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let repository = &scope.repository_instance_id;
        let row = tx.query_opt("SELECT envelope FROM alt_portable_exports WHERE repository=$1 AND export_id=$2 AND NOT retired AND verified_at IS NOT NULL", &[repository,&id])?.ok_or(Error::Precondition("active portable export not found"))?;
        let envelope: Envelope = serde_json::from_value(row.get(0))?;
        envelope.verify(scope, self.signing_key.verifying_key().as_bytes(), limits)?;
        let catalog = &envelope.catalog;
        let row = tx.query_one("SELECT transaction_sequence,default_branch,portable_freeze FROM alt_repositories WHERE id=$1", &[repository])?;
        let previous: Option<String> = row.get(2);
        if previous.as_deref().is_some_and(|old| old != id) {
            return Err(Error::Precondition(
                "another portable export freezes this repository",
            ));
        }
        if catalog.source != *scope
            || catalog.snapshot_sequence != unsigned(row.get(0))?
            || catalog.default_branch != row.get::<_, String>(1)
            || serde_json::to_value(&catalog.protections)?
                != serde_json::to_value(&current.policies)?
            || catalog
                .refs
                .iter()
                .map(|r| (r.name.clone(), r.oid.clone()))
                .collect::<BTreeMap<_, _>>()
                != current
                    .refs
                    .iter()
                    .map(|(name, oid)| (name.clone(), oid.to_string()))
                    .collect()
        {
            return Err(Error::Precondition(
                "source changed; capture a new portable export",
            ));
        }
        if previous.is_none() {
            tx.execute(
                "UPDATE alt_repositories SET portable_freeze=$2 WHERE id=$1",
                &[repository, &id],
            )?;
            audit(&mut tx, &auth.actor_id, "portable.export.frozen", scope, id)?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }

    /// Explicitly reopen this source after inspecting the destination. This
    /// never rolls back a destination or changes paid/service write authority.
    /// Current owners may also release a freeze preserved by disaster recovery.
    pub fn release_portable_freeze(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let repository = &scope.repository_instance_id;
        let previous: Option<String> = tx
            .query_one(
                "SELECT portable_freeze FROM alt_repositories WHERE id=$1",
                &[repository],
            )?
            .get(0);
        if previous.as_deref().is_some_and(|old| old != id) {
            return Err(Error::Precondition("source freeze identity changed"));
        }
        if previous.is_some() {
            tx.execute(
                "UPDATE alt_repositories SET portable_freeze=NULL WHERE id=$1",
                &[repository],
            )?;
            audit(
                &mut tx,
                &auth.actor_id,
                "portable.export.reopened",
                scope,
                id,
            )?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }

    /// Retire only this snapshot's pins. Source refs, task history and the
    /// durable export identity remain, while another export can reuse its slot.
    pub fn retire_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let repository = &scope.repository_instance_id;
        let frozen: Option<String> = tx
            .query_one(
                "SELECT portable_freeze FROM alt_repositories WHERE id=$1",
                &[repository],
            )?
            .get(0);
        if frozen.as_deref() == Some(id) {
            return Err(Error::Precondition(
                "explicitly release the source freeze before retiring its snapshot",
            ));
        }
        let row = tx
            .query_opt(
                "SELECT retired FROM alt_portable_exports WHERE repository=$1 AND export_id=$2",
                &[repository, &id],
            )?
            .ok_or(Error::Precondition("portable export not found"))?;
        if !row.get::<_, bool>(0) {
            tx.execute("UPDATE alt_portable_exports SET retired=true,documents='{}' WHERE repository=$1 AND export_id=$2", &[repository,&id])?;
            tx.execute(
                "DELETE FROM alt_pins WHERE repository=$1 AND pin_id=$2",
                &[repository, &pin_id(id)],
            )?;
            audit(
                &mut tx,
                &auth.actor_id,
                "portable.export.retired",
                scope,
                id,
            )?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}

impl Store {
    /// Query never captures a new snapshot. An obsolete scope cannot resume an
    /// export from before a restore, even if the original ID remains on disk.
    pub fn query_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: alt_portable::PackageLimits,
    ) -> Result<PortableExportState> {
        use PortableExportState as ExportState;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if scope.installation_id != self.spec.installation_id {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let unknown = || ExportState::Unknown {
            scope: scope.clone(),
            export_id: id.into(),
        };
        let result = if current.scope != *scope {
            unknown()
        } else if let Some(row) = tx.query_opt(
            "SELECT envelope,retired,verified_at FROM alt_portable_exports WHERE repository=$1 AND export_id=$2",
            &[&scope.repository_instance_id, &id],
        )? {
            let envelope: Envelope = serde_json::from_value(row.get(0))?;
            if envelope.catalog.source != *scope {
                unknown()
            } else {
                let digest = envelope.verify(scope, self.signing_key.verifying_key().as_bytes(), limits)?;
                if row.get::<_,bool>(1) {
                    ExportState::Retired { scope: scope.clone(), export_id: id.into(), catalog_digest: digest }
                } else if row.get::<_,Option<i64>>(2).is_none() {
                    ExportState::Preparing { scope: scope.clone(), export_id:id.into(), catalog_digest:digest, snapshot_sequence:envelope.catalog.snapshot_sequence }
                } else {
                    let frozen: Option<String> = tx.query_one("SELECT portable_freeze FROM alt_repositories WHERE id=$1", &[&scope.repository_instance_id])?.get(0);
                    ExportState::Available { envelope: Box::new(envelope), frozen: frozen.as_deref() == Some(id) }
                }
            }
        } else {
            ExportState::NotSeen { scope: scope.clone(), export_id: id.into() }
        };
        tx.commit()?;
        Ok(result)
    }
}

impl Store {
    /// Read-only archive inspection consumes bounded server resources under
    /// current administrator and repository-read authority, without a grant.
    pub fn check_portable_inspection(
        &mut self,
        credential: &Credential,
        scope: &Scope,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if current.scope != *scope || auth.attribution.kind != alt_remote_protocol::ActorKind::Human
        {
            return Err(Error::Unauthorized);
        }
        tx.commit()?;
        Ok(())
    }
}
