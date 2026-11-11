//! Capturing one immutable snapshot: the catalog, its documents and its retention.
use super::*;
use crate::Jsonb;
use alt_crypto::ed25519::SigningKey;
use alt_portable::{Catalog, HistoricalPublication, PackageLimits, Reference};
use alt_remote_protocol::{Publication, RequestState};

type Segments = (Vec<(String, bool)>, SegmentSet);

enum Capture {
    Captured(PortableExport),
    /// an earlier capture of this export, to be read through the named segments
    Recorded {
        envelope: Envelope,
        documents: BTreeMap<String, Vec<u8>>,
        locations: crate::segments::Locations,
        ready: bool,
        ids: Vec<(String, bool)>,
    },
    /// the live segments are not the ones that were opened
    Moved(Vec<(String, bool)>),
}

/// What a snapshot accumulates besides its Git objects.
struct Inventory {
    blobs: BTreeMap<String, Blob>,
    documents: BTreeMap<String, Vec<u8>>,
    identities: BTreeMap<(String, String), IdentityRequirement>,
    budget: usize,
}
impl Inventory {
    fn document(&mut self, value: &impl alt_json::ToJson) -> Result<Blob> {
        document(
            &mut self.documents,
            &mut self.blobs,
            value,
            &mut self.budget,
        )
    }
    fn actor(&mut self, installation: &str, actor: &str) {
        self.identities
            .entry((installation.into(), actor.into()))
            .or_insert_with(|| identity(installation, actor));
    }
}

/// Published content the content store holds. Previous export snapshots are not
/// included recursively: only live publication roots, including retained task
/// history, seed this one.
fn stored_content(
    tx: &mut Transaction<'_>,
    repository: &str,
    limits: PackageLimits,
) -> Result<BTreeMap<String, Blob>> {
    let bound = integer((limits.entries as u64).saturating_add(1))?;
    let rows = tx.query("SELECT b.blake3,b.bytes,b.state FROM alt_blobs b WHERE b.repository=$1 AND EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=b.repository AND p.blake3=b.blake3 AND p.expires_at IS NULL AND p.pin_id NOT LIKE 'portable-export:%') ORDER BY b.blake3 LIMIT $2", &[&repository,&bound])?;
    if rows.len() > limits.entries {
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
        blobs.insert(blob.blake3.clone(), blob);
    }
    Ok(blobs)
}

fn source_identities(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    limits: PackageLimits,
) -> Result<BTreeMap<(String, String), IdentityRequirement>> {
    let repository = &scope.repository_instance_id;
    let bound = integer((limits.entries as u64).saturating_add(1))?;
    let rows = tx.query("SELECT a.actor_id,COALESCE(g.can_read,false),COALESCE(g.publish_records,false),COALESCE(g.grants,'[]'::jsonb),COALESCE(r.can_review,false) FROM (SELECT actor_id FROM alt_grants WHERE repository=$1 UNION SELECT actor_id FROM alt_review_grants WHERE repository=$1) a LEFT JOIN alt_grants g ON g.repository=$1 AND g.actor_id=a.actor_id LEFT JOIN alt_review_grants r ON r.repository=$1 AND r.actor_id=a.actor_id ORDER BY a.actor_id LIMIT $2", &[repository,&bound])?;
    if rows.len() > limits.entries {
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
                ref_grants: row.get::<_, Jsonb>(3).read()?,
            },
        );
    }
    Ok(identities)
}

/// Task publications this installation committed, as logical documents.
fn committed_publications(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    key: &SigningKey,
    protocol: alt_remote_protocol::Limits,
    limits: PackageLimits,
    inventory: &mut Inventory,
) -> Result<Vec<HistoricalPublication>> {
    let bound = integer((limits.publications as u64).saturating_add(1))?;
    let rows = tx.query(concat!("SELECT q.publication,q.manifest,", crate::committed::state_sql!(), " FROM alt_requests q JOIN alt_task_bundles b ON b.repository=q.repository AND b.epoch=q.epoch AND b.actor_id=q.actor_id AND b.request_id=q.request_id WHERE q.repository=$1 AND q.state='committed' ORDER BY q.digest LIMIT $2"), &[&scope.repository_instance_id,&bound])?;
    if rows.len() > limits.publications {
        return Err(Error::Limit("portable task publication inventory"));
    }
    let mut publications = Vec::new();
    for row in rows {
        let publication: Publication = row.get::<_, Jsonb>(0).read()?;
        let manifest: alt_remote_protocol::Manifest = row.get::<_, Jsonb>(1).read()?;
        let RequestState::Committed { receipt } = row.get::<_, Jsonb>(2).read()? else {
            return Err(Error::Integrity("portable original receipt"));
        };
        let proofs = crate::provenance::load(tx, scope, &publication, &receipt, key, protocol)?;
        let origin = &publication.key.scope.installation_id;
        for actor in std::iter::once(&publication.key.actor_id)
            .chain(publication.attribution.on_behalf_of.iter())
        {
            inventory.actor(origin, actor);
        }
        publications.push(HistoricalPublication {
            digest: publication.digest(protocol)?,
            publication: inventory.document(&publication)?,
            manifest: inventory.document(&manifest)?,
            receipt: inventory.document(&receipt)?,
            proofs,
        });
    }
    Ok(publications)
}

/// Publications an earlier import brought in. Their original metadata payload
/// descriptors remain unchanged, including insignificant JSON whitespace in an
/// imported signed document.
fn archived_publications(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    key: &SigningKey,
    protocol: alt_remote_protocol::Limits,
    limits: PackageLimits,
    committed: usize,
) -> Result<Vec<HistoricalPublication>> {
    let bound = integer((limits.publications as u64).saturating_add(1))?;
    let rows = tx.query("SELECT publication,receipt,original FROM alt_archived_publications WHERE repository=$1 ORDER BY digest LIMIT $2", &[&scope.repository_instance_id,&bound])?;
    if rows.len().saturating_add(committed) > limits.publications {
        return Err(Error::Limit("portable historical publication inventory"));
    }
    let mut publications = Vec::new();
    for row in rows {
        let publication: Publication = row.get::<_, Jsonb>(0).read()?;
        let receipt: alt_remote_protocol::Receipt = row.get::<_, Jsonb>(1).read()?;
        let mut original: HistoricalPublication = row.get::<_, Jsonb>(2).read()?;
        original.proofs =
            crate::provenance::load(tx, scope, &publication, &receipt, key, protocol)?;
        publications.push(original);
    }
    Ok(publications)
}

/// Receipts of the imports this repository went through, with the identities
/// their envelopes and intents name.
fn import_history(
    tx: &mut Transaction<'_>,
    repository: &str,
    limits: PackageLimits,
    inventory: &mut Inventory,
) -> Result<Vec<Blob>> {
    let bound = integer((limits.entries as u64).saturating_add(1))?;
    let rows = tx.query("SELECT envelope,receipt FROM alt_portable_imports WHERE repository=$1 AND state='committed' ORDER BY import_id LIMIT $2", &[&repository,&bound])?;
    if rows.len() > limits.entries {
        return Err(Error::Limit("portable import history inventory"));
    }
    let mut receipts = BTreeMap::new();
    for row in rows {
        let original: Envelope = row.get::<_, Jsonb>(0).read()?;
        let receipt: alt_portable::ImportReceipt = row.get::<_, Jsonb>(1).read()?;
        for item in original.catalog.identities {
            let key = (item.installation_id.clone(), item.actor_id.clone());
            if let Some(existing) = inventory.identities.get_mut(&key) {
                // These are accumulated historical requirements, never
                // installed capabilities. Return migrations can encounter
                // the same qualified identity with changed permissions.
                existing.had_read |= item.had_read;
                existing.had_publish_records |= item.had_publish_records;
                existing.had_review |= item.had_review;
                let mut grants = BTreeMap::new();
                for grant in existing.ref_grants.iter().chain(&item.ref_grants) {
                    grants.insert(alt_json::to_string(grant)?, grant.clone());
                }
                existing.ref_grants = grants.into_values().collect();
            } else {
                inventory.identities.insert(key, item);
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
            inventory.actor(target, actor);
        }
        for blob in original.catalog.import_receipts {
            receipts.insert(blob.blake3.clone(), blob);
        }
        let blob = inventory.document(&receipt)?;
        receipts.insert(blob.blake3.clone(), blob);
    }
    Ok(receipts.into_values().collect())
}

impl Store {
    /// Persist the original metadata and retention in one short transaction.
    pub(super) fn capture_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: VerificationLimits,
    ) -> Result<(PortableExport, bool)> {
        limits.package.validate()?;
        let repository = &scope.repository_instance_id;
        // segment files are opened outside the transaction that lists them
        let mut opened = Segments::default();
        loop {
            match self.capture_with(credential, scope, id, limits, &opened)? {
                Capture::Captured(export) => return Ok((export, false)),
                Capture::Recorded {
                    envelope,
                    documents,
                    locations,
                    ready,
                    ids,
                } => {
                    let (_, segments) = self.open_listed_segments(repository, ids)?;
                    let export = PortableExport {
                        envelope,
                        documents,
                        bytes: self.bytes.clone(),
                        locations,
                        segments,
                    };
                    return Ok((export, ready));
                }
                Capture::Moved(ids) => opened = self.open_listed_segments(repository, ids)?,
            }
        }
    }

    fn capture_with(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: VerificationLimits,
        opened: &Segments,
    ) -> Result<Capture> {
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
        let ids = Self::live_segment_ids(&mut tx, repository)?;
        if let Some(row) = tx.query_opt(
            "SELECT actor_id,envelope,documents,retired,verified_at FROM alt_portable_exports WHERE repository=$1 AND export_id=$2",
            &[repository, &id],
        )? {
            if row.get::<_, String>(0) != auth.actor_id || row.get::<_, bool>(3) {
                return Err(Error::Precondition("export belongs to another actor or was retired"));
            }
            let envelope: Envelope = row.get::<_, Jsonb>(1).read()?;
            envelope.verify(scope, self.signing_key.verifying_key().as_bytes(), limits.package)?;
            let locations = export_locations(&mut tx, scope, &envelope.catalog.blobs)?;
            let documents = row.get::<_, Jsonb>(2).read()?;
            tx.commit()?;
            return Ok(Capture::Recorded {
                envelope,
                documents,
                locations,
                ready: row.get::<_, Option<i64>>(4).is_some(),
                ids,
            });
        }
        if ids != opened.0 {
            return Ok(Capture::Moved(ids));
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
        catalog.change_links = crate::link_history::list(
            &mut tx,
            repository,
            limits.package.entries,
            limits.package.metadata_bytes,
        )?;
        let mut inventory = Inventory {
            blobs: stored_content(&mut tx, repository, limits.package)?,
            documents: BTreeMap::new(),
            identities: source_identities(&mut tx, scope, limits.package)?,
            budget: limits.package.metadata_bytes,
        };
        let objects = objects::git_objects(&opened.1, limits.package.entries)?;
        for object in objects.values() {
            let payload = &object.payload;
            if let Some(old) = inventory
                .blobs
                .insert(payload.blake3.clone(), payload.clone())
                && old != *payload
            {
                return Err(Error::Integrity("portable object payload"));
            }
        }
        catalog.objects = objects.into_values().collect();
        catalog.publications = committed_publications(
            &mut tx,
            scope,
            &self.signing_key,
            self.limits,
            limits.package,
            &mut inventory,
        )?;
        catalog.publications.extend(archived_publications(
            &mut tx,
            scope,
            &self.signing_key,
            self.limits,
            limits.package,
            catalog.publications.len(),
        )?);
        catalog.publications.sort_by(|a, b| a.digest.cmp(&b.digest));
        catalog.import_receipts =
            import_history(&mut tx, repository, limits.package, &mut inventory)?;
        catalog.blobs = inventory.blobs.into_values().collect();
        catalog.identities = inventory.identities.into_values().collect();
        let locations = export_locations(&mut tx, scope, &catalog.blobs)?;
        let result = PortableExport {
            locations,
            envelope: Envelope::sign(catalog, &self.signing_key, limits.package)?,
            documents: inventory.documents,
            bytes: self.bytes.clone(),
            segments: opened.1.clone(),
        };
        // Only what has to stay in the content store is retained there: Git objects
        // are held by the segments this export reads them from.
        for blob in result.stored(limits.package)? {
            crate::requests::pin(&mut tx, repository, &pin_id(id), &blob.blake3, None)?;
        }
        // Capturing metadata cannot extend an expired authority lease.
        require_admin(&mut tx, &self.spec, credential)?;
        authorize(&mut tx, &self.spec, credential, &current)?;
        let created_at = integer(now(&mut tx)?)?;
        tx.execute("INSERT INTO alt_portable_exports(repository,export_id,actor_id,envelope,documents,created_at) VALUES($1,$2,$3,$4,$5,$6)", &[repository,&id,&auth.actor_id,&Jsonb::from(&result.envelope)?,&Jsonb::from(&result.documents)?,&created_at])?;
        audit(
            &mut tx,
            &auth.actor_id,
            "portable.export.preparing",
            scope,
            id,
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(Capture::Captured(result))
    }
}
