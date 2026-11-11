use super::*;
use crate::Jsonb;
use alt_portable::{
    BlobSource, HistoricalPublication, ImportReceiptBody, VerificationLimits, VerifiedSnapshot,
};
use alt_remote_protocol::{
    Manifest, Publication, Receipt, TransferBody, TransferProof, TransferReason,
};
use std::{collections::BTreeMap, io::Read, os::unix::fs::MetadataExt};

mod settle;

type Stamp = (u64, u64, u64, i64, i64, i64, i64);
struct Source {
    bytes: crate::ByteStore,
    target: Scope,
    blobs: BTreeMap<String, Blob>,
    locations: crate::segments::Locations,
}
impl BlobSource for Source {
    fn open(&self, blob: &Blob) -> alt_portable::Result<Box<dyn Read + '_>> {
        if self.blobs.get(&blob.blake3) != Some(blob) {
            return Err(alt_portable::Error::Invalid(
                "import read outside selection",
            ));
        }
        Ok(Box::new(
            self.bytes
                .read_payload(
                    &self.target,
                    &blob.blake3,
                    blob.bytes,
                    self.locations.get(&blob.blake3),
                )
                .map_err(|e| std::io::Error::other(e.to_string()))?,
        ))
    }
}
struct History {
    original: HistoricalPublication,
    publication: Publication,
    manifest: Manifest,
    receipt: Receipt,
}
/// Only full semantic verification constructs this value. Activation rechecks
/// the immutable intent, live authority, content identities and temporary pins.
pub struct PortableImportWork {
    intent: ImportIntent,
    envelope: Envelope,
    verified: VerifiedSnapshot,
    history: Vec<History>,
    stamps: Vec<Stamp>,
    locations: crate::segments::Locations,
    limits: PackageLimits,
}
fn stamps(
    bytes: &crate::ByteStore,
    scope: &Scope,
    blobs: &[Blob],
    locations: &crate::segments::Locations,
) -> Result<Vec<Stamp>> {
    blobs
        .iter()
        .map(|b| {
            let m = bytes
                .read_payload(scope, &b.blake3, b.bytes, locations.get(&b.blake3))?
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
fn complete(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
    digest: &str,
    envelope: &Envelope,
    time: u64,
) -> Result<()> {
    let rows = tx.query("SELECT c.blake3,b.bytes FROM alt_portable_import_content c JOIN alt_blobs b ON b.repository=c.repository AND b.blake3=c.blake3 JOIN alt_pins p ON p.repository=b.repository AND p.blake3=b.blake3 AND p.pin_id=$3 WHERE c.repository=$1 AND c.import_id=$2 AND c.verified AND b.state='stored' AND b.accounted AND p.expires_at>$4 ORDER BY c.blake3", &[&key.scope.repository_instance_id,&key.request_id,&pin_id(digest),&integer(time)?])?;
    let actual = rows
        .iter()
        .map(|r| {
            Ok(Blob {
                blake3: r.get(0),
                bytes: unsigned(r.get(1))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if actual != envelope.catalog.blobs {
        return Err(Error::Precondition(
            "portable import content incomplete or expired",
        ));
    }
    Ok(())
}
impl Store {
    /// Validation runs after releasing the repository and Cloud authority SQL
    /// locks. An expired pin, permission change or restore invalidates this work.
    pub fn verify_portable_import(
        &mut self,
        credential: &Credential,
        intent: &ImportIntent,
        limits: VerificationLimits,
    ) -> Result<PortableImportWork> {
        crate::telemetry::request(&intent.key);
        crate::telemetry::observe(alt_server_telemetry::Phase::Validate, || {
            let key = &intent.key;
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
            same(&row, intent, limits.package)?;
            let time = now(&mut tx)?;
            active(&row, key, time)?;
            empty(&mut tx, &key.scope.repository_instance_id)?;
            mappings(&mut tx, intent)?;
            let envelope: Envelope = row.get::<_, Jsonb>("envelope").read()?;
            intent.validate_envelope(&envelope, limits.package)?;
            complete(
                &mut tx,
                key,
                &row.get::<_, String>("digest"),
                &envelope,
                time,
            )?;
            crate::cloud_quota::check_publication(&mut tx, &self.spec)?;
            let locations = crate::segments::locations(
                &mut tx,
                &key.scope.repository_instance_id,
                &envelope
                    .catalog
                    .blobs
                    .iter()
                    .map(|b| b.blake3.as_str())
                    .collect::<Vec<_>>(),
            )?;
            tx.commit()?;
            let before = stamps(&self.bytes, &key.scope, &envelope.catalog.blobs, &locations)?;
            let source = Source {
                locations: locations.clone(),
                bytes: self.bytes.clone(),
                target: key.scope.clone(),
                blobs: envelope
                    .catalog
                    .blobs
                    .iter()
                    .map(|b| (b.blake3.clone(), b.clone()))
                    .collect(),
            };
            let public_key = alt_provenance::decode_hex(&intent.source_public_key)
                .map_err(|_| Error::Integrity("portable source key"))?;
            let verified =
                VerifiedSnapshot::verify(&envelope, &intent.source, &public_key, &source, limits)?;
            let history = envelope
                .catalog
                .publications
                .iter()
                .map(|item| {
                    let publication = Publication::decode(
                        &alt_portable::read_metadata(
                            &source,
                            &item.publication,
                            limits.package.metadata_bytes,
                        )?,
                        limits.package.protocol,
                    )?;
                    let manifest = Manifest::decode(
                        &alt_portable::read_metadata(
                            &source,
                            &item.manifest,
                            limits.package.metadata_bytes,
                        )?,
                        limits.package.protocol,
                    )?;
                    let receipt = alt_json::from_slice(&alt_portable::read_metadata(
                        &source,
                        &item.receipt,
                        limits.package.metadata_bytes,
                    )?)?;
                    Ok(History {
                        original: item.clone(),
                        publication,
                        manifest,
                        receipt,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            if before != stamps(&self.bytes, &key.scope, &envelope.catalog.blobs, &locations)? {
                return Err(Error::Precondition(
                    "portable content changed during validation",
                ));
            }
            Ok(PortableImportWork {
                intent: intent.clone(),
                envelope,
                verified,
                history,
                stamps: before,
                locations,
                limits: limits.package,
            })
        })
    }

    /// Commit the complete snapshot as one transaction. Replaying a committed
    /// intent returns its durable receipt without overwriting subsequent work.
    pub fn activate_portable_import(
        &mut self,
        credential: &Credential,
        work: &PortableImportWork,
    ) -> Result<ImportReceipt> {
        let repository = &work.intent.key.scope.repository_instance_id;
        let materialized = self.materialize_import(work)?;
        let result = self.activate_transaction(credential, work, materialized.as_ref());
        match (&result, materialized) {
            (Ok((_, true)), Some(segment)) => {
                self.adopt_segment(repository, &segment.id, segment.segment)?
            }
            // the transaction may have committed: the sweep removes the file if it did not
            (Err(Error::OutcomeUnknown), Some(_)) => {}
            (_, Some(segment)) => {
                // a replayed or failed activation references no new segment
                let _ = std::fs::remove_file(&segment.path);
            }
            _ => {}
        }
        result.map(|(receipt, _)| receipt)
    }
    /// the receipt, and whether this call was the one that activated the import
    fn activate_transaction(
        &mut self,
        credential: &Credential,
        work: &PortableImportWork,
        materialized: Option<&crate::packs::Materialized>,
    ) -> Result<(ImportReceipt, bool)> {
        let intent = &work.intent;
        let key = &intent.key;
        let repository = &key.scope.repository_instance_id;
        let digest = intent.digest(work.limits)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (current, auth) = authorize_read(&mut tx, &self.spec, credential, key)?;
        let row = row(&mut tx, key)?.ok_or(Error::Precondition("portable import not found"))?;
        same(&row, intent, work.limits)?;
        if let PortableImportState::Committed { receipt } =
            state(&mut tx, &row, key, &current.scope)?
        {
            tx.commit()?;
            return Ok((*receipt, false));
        }
        authorize_write(
            &mut tx,
            &self.spec,
            key,
            &current,
            &auth,
            Some(&key.request_id),
        )?;
        let time = now(&mut tx)?;
        active(&row, key, time)?;
        empty(&mut tx, repository)?;
        mappings(&mut tx, intent)?;
        let envelope: Envelope = row.get::<_, Jsonb>("envelope").read()?;
        intent.validate_envelope(&envelope, work.limits)?;
        if work.verified.catalog_digest() != intent.catalog_digest
            || envelope.catalog.digest(work.limits)? != work.envelope.catalog.digest(work.limits)?
            || intent.target_public_key
                != alt_provenance::encode_hex(self.signing_key.verifying_key().as_bytes())
        {
            return Err(Error::Integrity("portable verified work binding"));
        }
        complete(&mut tx, key, &digest, &envelope, time)?;
        if work.stamps
            != stamps(
                &self.bytes,
                &key.scope,
                &envelope.catalog.blobs,
                &work.locations,
            )?
        {
            return Err(Error::Precondition("portable verified content changed"));
        }
        crate::cloud_quota::check_publication(&mut tx, &self.spec)?;
        settle::retain_history(&mut tx, repository, &pin_id(&digest), work)?;
        for reference in &envelope.catalog.refs {
            tx.execute(
                "INSERT INTO alt_refs(repository,name,oid) VALUES($1,$2,$3)",
                &[repository, &reference.name, &reference.oid],
            )?;
        }
        for history in &work.history {
            let original = &history.original;
            let mut proofs = original.proofs.clone();
            let previous_proof = proofs.last().map(TransferProof::digest).transpose()?;
            let export = envelope
                .exports
                .iter()
                .find(|e| e.body.publication_digest == original.digest)
                .ok_or(Error::Integrity("portable publication certificate"))?
                .clone();
            proofs.push(
                alt_provenance::sign_transfer(
                    TransferBody {
                        version: 1,
                        publication_digest: original.digest.clone(),
                        source: intent.source.clone(),
                        target: key.scope.clone(),
                        source_public_key: intent.source_public_key.clone(),
                        target_public_key: intent.target_public_key.clone(),
                        operation_id: digest.clone(),
                        previous_proof,
                        reason: TransferReason::Migration {
                            export: Box::new(export),
                        },
                    },
                    &self.signing_key,
                )
                .map_err(|_| Error::Integrity("portable migration proof"))?,
            );
            alt_provenance::verify_origin(
                &history.publication,
                &history.receipt,
                &proofs,
                &key.scope,
                self.signing_key.verifying_key().as_bytes(),
                work.limits.protocol,
            )
            .map_err(|_| Error::Integrity("portable migration chain"))?;
            tx.execute("INSERT INTO alt_archived_publications(repository,digest,publication,manifest,receipt,original) VALUES($1,$2,$3,$4,$5,$6)", &[repository,&original.digest,&Jsonb::from(&history.publication)?,&Jsonb::from(&history.manifest)?,&Jsonb::from(&history.receipt)?,&Jsonb::from(&original)?])?;
            crate::provenance::save(&mut tx, &key.scope, &original.digest, &proofs)?;
            let verified = &work
                .verified
                .publications()
                .iter()
                .find(|(d, _)| d == &original.digest)
                .ok_or(Error::Integrity("portable verified publication"))?
                .1;
            if let Some(tasks) = &verified.tasks {
                for selection in tasks.records() {
                    tx.execute("INSERT INTO alt_task_records(repository,kind,record_id,payload_blake3) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING", &[repository,&selection.record.kind.as_str(),&selection.record.id,&selection.payload.blake3])?;
                    tx.execute("INSERT INTO alt_archived_records(repository,digest,kind,record_id,payload_blake3) VALUES($1,$2,$3,$4,$5)", &[repository,&original.digest,&selection.record.kind.as_str(),&selection.record.id,&selection.payload.blake3])?;
                }
                for root in tasks.roots() {
                    let kind = alt_json::to_value(root.kind)?
                        .as_str()
                        .ok_or(Error::Integrity("portable root kind"))?
                        .to_owned();
                    tx.execute("INSERT INTO alt_task_roots(repository,kind,record_id,publication_digest) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING", &[repository,&kind,&root.id,&original.digest])?;
                }
            }
        }
        if let Some(segment) = materialized {
            crate::pack_native::account_segment(&mut tx, repository, segment.bytes)?;
            crate::packs::insert_segment(
                &mut tx,
                repository,
                1,
                &crate::NewSegment {
                    id: &segment.id,
                    objects: segment.objects,
                    bytes: segment.bytes,
                    flags: segment.flags,
                    inline: segment.inline.as_deref(),
                },
            )?;
        }
        crate::link_history::import(
            &mut tx,
            repository,
            &envelope.catalog,
            unsigned(row.get("reserved_bytes"))?,
        )?;
        tx.execute("UPDATE alt_repositories SET transaction_sequence=1,policy_version=policy_version+1,policies=$2,default_branch=$3,reserved_bytes=reserved_bytes-$4,portable_import=NULL WHERE id=$1", &[repository,&Jsonb::from(&envelope.catalog.protections)?,&envelope.catalog.default_branch,&row.get::<_,i64>("reserved_bytes")])?;
        crate::cloud_quota::release_portable(&mut tx, key)?;
        let receipt = ImportReceipt::sign(
            ImportReceiptBody {
                version: 1,
                intent: intent.clone(),
                transaction_sequence: 1,
                committed_at_unix_ms: now(&mut tx)?,
            },
            &self.signing_key,
            work.limits,
        )?;
        tx.execute("UPDATE alt_portable_imports SET state='committed',receipt=$3,reserved_bytes=0 WHERE repository=$1 AND import_id=$2", &[repository,&key.request_id,&Jsonb::from(&receipt)?])?;
        let event = Jsonb(alt_json::json!({"kind":"portable_import_committed","receipt":receipt}));
        tx.execute(
            "INSERT INTO alt_audit(repository,sequence,actor_id,event) VALUES($1,1,$2,$3)",
            &[repository, &auth.actor_id, &event],
        )?;
        tx.execute(
            "INSERT INTO alt_outbox(repository,sequence,event) VALUES($1,1,$2)",
            &[repository, &event],
        )?;
        // SQL work can outlive a short Cloud lease. Repeat authorization before
        // commit using the retained import lock expectation in the transaction.
        require_admin(&mut tx, &self.spec, credential)?;
        let renewed = authorize(&mut tx, &self.spec, credential, &current)?;
        authorize_write(&mut tx, &self.spec, key, &current, &renewed, None)?;
        crate::cloud_quota::check_publication(&mut tx, &self.spec)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok((receipt, true))
    }
}
