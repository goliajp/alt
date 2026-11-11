//! Logical, authenticated snapshots. Physical disaster backups stay separate.
use crate::Jsonb;
mod capture;
mod lifecycle;
mod objects;

use crate::database::Transaction;
use crate::store::{authorize, instance, integer, now, require_admin, snapshot, unsigned};
use crate::{ByteStore, Credential, Error, Result, Store};
use alt_pack_segment::SegmentSet;
use alt_portable::{Blob, Envelope, IdentityRequirement, VerificationLimits, VerifiedSnapshot};
use alt_remote_protocol::{RequestKey, Scope};

use std::collections::BTreeMap;

pub use alt_portable::http::ExportState as PortableExportState;

/// An authorized, pinned snapshot. Possessing its ID alone never permits a new
/// download: obtaining another handle rechecks the current account and scope.
pub struct PortableExport {
    envelope: Envelope,
    documents: BTreeMap<String, Vec<u8>>,
    bytes: ByteStore,
    locations: crate::segments::Locations,
    /// where the Git objects are read from; a merge keeps every object it replaces
    segments: SegmentSet,
}
/// Completed byte validation, bound to immutable intent and byte-store inodes.
/// This grants no authority; completion rechecks current permissions and epoch.
pub struct PortableExportWork {
    export: PortableExport,
    /// the payloads the content store has to hold for this export
    stored: Vec<Blob>,
    stamps: Vec<objects::Stamp>,
    limits: VerificationLimits,
}
impl PortableExport {
    pub fn envelope(&self) -> &Envelope {
        &self.envelope
    }
}

fn export_locations(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    blobs: &[Blob],
) -> Result<crate::segments::Locations> {
    crate::segments::locations(
        tx,
        &scope.repository_instance_id,
        &blobs.iter().map(|b| b.blake3.as_str()).collect::<Vec<_>>(),
    )
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
    value: &impl alt_json::ToJson,
    budget: &mut usize,
) -> Result<Blob> {
    let raw = alt_json::to_vec(value)?;
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
        &Jsonb(alt_json::json!({"export_id":id,"scope":scope})),
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
        crate::telemetry::scope(scope);
        if let Some(trace) = alt_server_telemetry::Trace::current() {
            trace.identity(None, alt_server_telemetry::Id::new(id), None);
        }
        crate::telemetry::observe(alt_server_telemetry::Phase::Validate, || {
            let (result, ready) = self.capture_portable_export(credential, scope, id, limits)?;
            let stored = result.stored(limits.package)?;
            let before = result.stamps(&self.bytes, &stored)?;
            if !ready {
                VerifiedSnapshot::verify(
                    &result.envelope,
                    scope,
                    self.signing_key.verifying_key().as_bytes(),
                    &result,
                    limits,
                )?;
            }
            if before != result.stamps(&self.bytes, &stored)? {
                return Err(Error::Precondition(
                    "portable export bytes changed during validation",
                ));
            }
            Ok(PortableExportWork {
                export: result,
                stored,
                stamps: before,
                limits,
            })
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
            stored,
            stamps,
            limits,
        } = work;
        if stamps != export.stamps(&self.bytes, &stored)? {
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
        let recorded: Envelope = row.get::<_, Jsonb>(1).read()?;
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
        let retained = rows
            .iter()
            .map(|r| {
                Ok(Blob {
                    blake3: r.get(0),
                    bytes: unsigned(r.get(1))?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if retained != stored {
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
}
