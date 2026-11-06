//! Physical-operator maintenance primitives. A snapshot owns the instance row
//! lock until all metadata and immutable bytes have been exported. Publication,
//! credential changes and GC take the shared side of this same barrier.
use crate::store::{instance, integer, unsigned};
use crate::{ByteStore, Error, InstanceSpec, Result, Store};
use alt_remote_protocol::Scope;
use ed25519_dalek::SigningKey;
use postgres::{Client, Transaction};
use serde::{Deserialize, Serialize};
use std::fs::File;

/// Counters are decimal strings to preserve the full database integer range in
/// browser clients. This is a read-only database observation, not a byte scrub
/// or proof that an external backup remains available.
#[derive(Clone, Debug, Serialize)]
pub struct InstanceStatus {
    pub installation_id: String,
    pub runtime_generation: String,
    pub schema: u32,
    pub observed_at_unix_ms: String,
    pub repositories: String,
    pub stored_bytes: String,
    pub reserved_bytes: String,
    pub pending_requests: String,
    pub recovery_gaps: String,
    pub pending_events: String,
    pub last_restore: Option<InstanceAuditEntry>,
}
#[derive(Clone, Debug, Serialize)]
pub struct InstanceAuditEntry {
    pub sequence: String,
    pub actor_id: String,
    pub action: String,
    pub subject_id: String,
    pub occurred_at_unix_ms: String,
}
#[derive(Clone, Copy)]
pub enum BackupEvent {
    Started,
    Created,
    Unconfirmed,
}
impl Store {
    /// Physical operator authority: opening a Store requires the instance key
    /// and database access. This is deliberately not an HTTP mutation API.
    pub fn record_backup_event(&mut self, operation: &str, event: BackupEvent) -> Result<()> {
        alt_task::check_id(operation).map_err(|_| Error::Precondition("backup operation id"))?;
        let action = match event {
            BackupEvent::Started => "backup.started",
            BackupEvent::Created => "backup.created",
            BackupEvent::Unconfirmed => "backup.unconfirmed",
        };
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        tx.execute("INSERT INTO alt_instance_audit(actor_id,action,subject_id,details,occurred_at) VALUES('local-operator',$1,$2,'{}',floor(extract(epoch FROM clock_timestamp())*1000)::bigint)", &[&action, &operation])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
}
fn audit_entry(row: postgres::Row) -> Result<InstanceAuditEntry> {
    Ok(InstanceAuditEntry {
        sequence: unsigned(row.get(0))?.to_string(),
        actor_id: row.get(1),
        action: row.get(2),
        subject_id: row.get(3),
        occurred_at_unix_ms: unsigned(row.get(4))?.to_string(),
    })
}

impl Store {
    pub fn instance_status(&mut self, admin: &crate::Credential) -> Result<InstanceStatus> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        crate::store::require_admin(&mut tx, &self.spec, admin)?;
        // One statement gives these counters one MVCC snapshot. Old unfinished
        // requests are recovery gaps, never current queue work.
        let row = tx.query_one("SELECT \
            floor(extract(epoch FROM clock_timestamp())*1000)::bigint, \
            (SELECT count(*) FROM alt_repositories), \
            (SELECT COALESCE(sum(bytes),0)::text FROM alt_blobs WHERE state='stored'), \
            (SELECT COALESCE(sum(reserved_bytes),0)::text FROM alt_repositories), \
            (SELECT count(*) FROM alt_requests q JOIN alt_repositories r ON q.repository=r.id WHERE q.state IN ('receiving','ready') AND q.epoch=r.recovery_epoch), \
            (SELECT count(*) FROM alt_requests q JOIN alt_repositories r ON q.repository=r.id WHERE q.state IN ('receiving','ready') AND q.epoch<>r.recovery_epoch), \
            (SELECT count(*) FROM alt_outbox WHERE NOT delivered)", &[])?;
        let last_restore = tx.query_opt("SELECT sequence,actor_id,action,subject_id,occurred_at FROM alt_instance_audit WHERE action='instance.restore' ORDER BY sequence DESC LIMIT 1", &[])?.map(audit_entry).transpose()?;
        let result = InstanceStatus {
            installation_id: self.spec.installation_id.clone(),
            runtime_generation: self.spec.runtime_generation.to_string(),
            schema: crate::SCHEMA_VERSION,
            observed_at_unix_ms: unsigned(row.get(0))?.to_string(),
            repositories: unsigned(row.get(1))?.to_string(),
            stored_bytes: row.get(2),
            reserved_bytes: row.get(3),
            pending_requests: unsigned(row.get(4))?.to_string(),
            recovery_gaps: unsigned(row.get(5))?.to_string(),
            pending_events: unsigned(row.get(6))?.to_string(),
            last_restore,
        };
        tx.commit()?;
        Ok(result)
    }

    /// Audit metadata only: action details can contain operator reasons and are
    /// deliberately excluded from the browser/CLI history projection.
    pub fn instance_audit(
        &mut self,
        admin: &crate::Credential,
        before: Option<u64>,
        limit: u32,
    ) -> Result<crate::Page<InstanceAuditEntry>> {
        if !(1..=100).contains(&limit) || before == Some(0) {
            return Err(Error::Precondition("audit page bounds"));
        }
        let before = before.map(integer).transpose()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        crate::store::require_admin(&mut tx, &self.spec, admin)?;
        let mut items = tx.query("SELECT sequence,actor_id,action,subject_id,occurred_at FROM alt_instance_audit WHERE ($1::bigint IS NULL OR sequence<$1) ORDER BY sequence DESC LIMIT $2", &[&before, &(i64::from(limit)+1)])?.into_iter().map(audit_entry).collect::<Result<Vec<_>>>()?;
        let more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next = if more {
            items.last().map(|entry| entry.sequence.clone())
        } else {
            None
        };
        tx.commit()?;
        Ok(crate::Page { items, next })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotBlob {
    pub repository: String,
    pub blake3: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRepository {
    pub repository: String,
    pub recovery_epoch: u64,
    pub transaction_sequence: u64,
    pub refs: std::collections::BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotCatalog {
    pub schema: u32,
    pub instance: InstanceSpec,
    pub repositories: Vec<SnapshotRepository>,
    pub blobs: Vec<SnapshotBlob>,
}

pub struct MaintenanceSnapshot<'a> {
    _transaction: Transaction<'a>,
    bytes: ByteStore,
    catalog: SnapshotCatalog,
    exported_snapshot: String,
}
impl<'a> MaintenanceSnapshot<'a> {
    /// Supports precisely the checksummed historical layouts, so the current
    /// operator can make an encrypted pre-upgrade backup of an older instance.
    /// No mutations or schema changes occur here. Dropping releases the barrier.
    pub fn begin(
        client: &'a mut Client,
        bytes: ByteStore,
        spec: &InstanceSpec,
        signing_key: &SigningKey,
        schema: u32,
    ) -> Result<Self> {
        if !(1..=crate::SCHEMA_VERSION).contains(&schema) {
            return Err(Error::Precondition("unsupported snapshot schema"));
        }
        let mut tx = client.transaction()?;
        // This lock is also used by migrations. Take it before inspecting the
        // schema to prevent a concurrent migration changing our table set.
        tx.query_one("SELECT pg_advisory_xact_lock(715071909111)", &[])?;
        crate::schema::check(&mut tx, schema as usize)?;
        tx.query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
            &[],
        )?;
        instance(&mut tx, spec, signing_key)?;
        let exported_snapshot = tx.query_one("SELECT pg_export_snapshot()", &[])?.get(0);
        let mut repositories = Vec::new();
        for row in tx.query(
            "SELECT id,recovery_epoch,transaction_sequence FROM alt_repositories ORDER BY id",
            &[],
        )? {
            let repository: String = row.get(0);
            let mut refs = std::collections::BTreeMap::new();
            for reference in tx.query(
                "SELECT name,oid FROM alt_refs WHERE repository=$1 ORDER BY name",
                &[&repository],
            )? {
                refs.insert(reference.get(0), reference.get(1));
            }
            repositories.push(SnapshotRepository {
                repository,
                recovery_epoch: unsigned(row.get(1))?,
                transaction_sequence: unsigned(row.get(2))?,
                refs,
            });
        }
        // A permanent pin to an absent or incomplete blob is a broken backup,
        // never a reason to silently omit a visible object or task attachment.
        let broken: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM alt_pins p LEFT JOIN alt_blobs b ON b.repository=p.repository AND b.blake3=p.blake3 WHERE p.expires_at IS NULL AND (b.state IS DISTINCT FROM 'stored'))", &[])?.get(0);
        if broken {
            return Err(Error::Integrity("published snapshot bytes unavailable"));
        }
        let mut blobs = Vec::new();
        for row in tx.query("SELECT repository,blake3,bytes FROM alt_blobs WHERE state='stored' ORDER BY repository,blake3", &[])? {
            let blob = SnapshotBlob { repository: row.get(0), blake3: row.get(1), bytes: unsigned(row.get(2))? };
            Scope { installation_id: spec.installation_id.clone(), repository_instance_id: blob.repository.clone(), recovery_epoch: 1 }.validate()?;
            crate::bytes::check_digest(&blob.blake3)?;
            blobs.push(blob);
        }
        Ok(Self {
            _transaction: tx,
            bytes,
            exported_snapshot,
            catalog: SnapshotCatalog {
                schema,
                instance: spec.clone(),
                repositories,
                blobs,
            },
        })
    }
    pub fn catalog(&self) -> &SnapshotCatalog {
        &self.catalog
    }
    pub fn exported_snapshot(&self) -> &str {
        &self.exported_snapshot
    }
    pub fn read_verified(&self, blob: &SnapshotBlob) -> Result<File> {
        if self
            .catalog
            .blobs
            .binary_search_by(|item| {
                (&item.repository, &item.blake3).cmp(&(&blob.repository, &blob.blake3))
            })
            .ok()
            .is_none_or(|i| &self.catalog.blobs[i] != blob)
        {
            return Err(Error::Precondition("blob is not part of this snapshot"));
        }
        self.bytes.verify(
            &Scope {
                installation_id: self.catalog.instance.installation_id.clone(),
                repository_instance_id: blob.repository.clone(),
                recovery_epoch: 1,
            },
            &blob.blake3,
            blob.bytes,
        )
    }
}

impl Store {
    /// Run only against an isolated, restored database after external fencing
    /// of the old service. The operator supplies a generation strictly newer
    /// than every source epoch. This cannot fence an independent remote DB.
    pub fn activate_restored(&mut self, generation: u64, backup_digest: &str) -> Result<()> {
        self.activate_restored_inner(generation, backup_digest, None)
    }
    /// A current, independently challenged runtime probe must prove which
    /// authenticated archive was activated, not merely that a fresh DB opens.
    pub fn check_restored_backup(&mut self, backup_digest: &str) -> Result<()> {
        crate::bytes::check_digest(backup_digest)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let expected = serde_json::json!({"backup_digest":backup_digest,"generation":self.spec.runtime_generation});
        let found: bool = tx.query_one(
            "SELECT EXISTS(SELECT 1 FROM alt_instance_audit WHERE action='instance.restore' AND subject_id=$1 AND details @> $2)",
            &[&self.spec.installation_id, &expected],
        )?.get(0);
        if !found {
            return Err(Error::Precondition("restored backup identity"));
        }
        Ok(())
    }
    /// Atomically replace Cloud authority, advance all recovery epochs and
    /// revoke credentials. The caller must first fence the source externally.
    pub fn activate_restored_cloud(
        &mut self,
        generation: u64,
        backup_digest: &str,
        binding: crate::CloudRestoreBinding<'_>,
    ) -> Result<()> {
        self.activate_restored_inner(generation, backup_digest, Some(binding))
    }
    fn activate_restored_inner(
        &mut self,
        generation: u64,
        backup_digest: &str,
        cloud: Option<crate::CloudRestoreBinding<'_>>,
    ) -> Result<()> {
        crate::bytes::check_digest(backup_digest)?;
        let mut tx = self.client.transaction()?;
        tx.query_one("SELECT pg_advisory_xact_lock(715071909111)", &[])?;
        tx.query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
            &[],
        )?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let max_epoch: i64 = tx
            .query_one(
                "SELECT COALESCE(max(recovery_epoch),0) FROM alt_repositories",
                &[],
            )?
            .get(0);
        if generation <= self.spec.runtime_generation || generation <= unsigned(max_epoch)? {
            return Err(Error::Precondition(
                "restore generation must exceed every source epoch",
            ));
        }
        integer(generation)?;
        crate::provenance::append_recovery(
            &mut tx,
            &self.spec,
            &self.signing_key,
            self.limits,
            generation,
            backup_digest,
        )?;
        match (self.spec.deployment_mode, cloud) {
            (alt_remote_protocol::DeploymentMode::Cloud, Some(binding)) => {
                crate::cloud::restore_binding(&mut tx, &self.spec, generation, &binding)?;
            }
            (alt_remote_protocol::DeploymentMode::SelfHosted, None) => {}
            _ => {
                return Err(Error::Precondition(
                    "recovery deployment authority mismatch",
                ));
            }
        }
        // A snapshot's unfinished request may have committed on the lost
        // source AFTER this watermark. Retain its original observed state;
        // query() reports these old-epoch gaps as Unknown, never Expired.
        // Reservations and temporary pins belong to the fenced incarnation.
        tx.batch_execute("DELETE FROM alt_device_authorizations; DELETE FROM alt_cloud_logins; DELETE FROM alt_cloud_reservations; DELETE FROM alt_cloud_portable_reservations; UPDATE alt_cloud_ingress SET reserved_bytes=0; UPDATE alt_cloud_binding SET service=NULL;")?;
        tx.batch_execute("UPDATE alt_requests SET reserved_bytes=0; UPDATE alt_portable_imports SET reserved_bytes=0; UPDATE alt_repositories SET reserved_bytes=0,portable_import=NULL")?;
        tx.batch_execute("UPDATE alt_requests SET fence=fence+1,worker_lease_until=0; UPDATE alt_credentials SET revoked=true; UPDATE alt_web_accounts SET generation=generation+1; DELETE FROM alt_web_sessions; DELETE FROM alt_login_budgets; DELETE FROM alt_pins WHERE expires_at IS NOT NULL")?;
        tx.execute("UPDATE alt_repositories SET recovery_epoch=$1,revocation_generation=revocation_generation+1", &[&integer(generation)?])?;
        tx.execute(
            "UPDATE alt_instance SET runtime_generation=$1 WHERE singleton",
            &[&integer(generation)?],
        )?;
        let details = serde_json::json!({"backup_digest":backup_digest,"previous_generation":self.spec.runtime_generation,"generation":generation});
        tx.execute("INSERT INTO alt_instance_audit(actor_id,action,subject_id,details,occurred_at) VALUES('local-operator','instance.restore',$1,$2,floor(extract(epoch FROM clock_timestamp())*1000)::bigint)", &[&self.spec.installation_id, &details])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        self.spec.runtime_generation = generation;
        Ok(())
    }
}
