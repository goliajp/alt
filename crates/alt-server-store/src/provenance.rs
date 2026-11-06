//! Durable lineage is appended under the same transaction as recovery fencing.
//! Reads verify it and never manufacture missing history or current approvals.
use crate::{Error, InstanceSpec, Result};
use alt_remote_protocol::{
    Limits, Publication, Receipt, Scope, TransferBody, TransferProof, TransferReason,
};
use ed25519_dalek::SigningKey;
use postgres::Transaction;

fn evidence<T>(value: alt_provenance::Result<T>) -> Result<T> {
    value.map_err(|_| Error::Integrity("publication provenance"))
}
fn stored(tx: &mut Transaction<'_>, scope: &Scope, digest: &str) -> Result<Vec<TransferProof>> {
    tx.query_opt("SELECT proofs FROM alt_publication_provenance WHERE repository=$1 AND publication_digest=$2", &[&scope.repository_instance_id, &digest])?
        .map(|row| serde_json::from_value(row.get(0)).map_err(Error::from)).transpose().map(|v|v.unwrap_or_default())
}
pub(crate) fn save(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    digest: &str,
    proofs: &[TransferProof],
) -> Result<()> {
    if proofs.is_empty() {
        return Ok(());
    }
    tx.execute("INSERT INTO alt_publication_provenance(repository,publication_digest,proofs) VALUES($1,$2,$3) ON CONFLICT(repository,publication_digest) DO UPDATE SET proofs=excluded.proofs", &[&scope.repository_instance_id,&digest,&serde_json::to_value(proofs)?])?;
    Ok(())
}
pub(crate) fn load(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    publication: &Publication,
    receipt: &Receipt,
    key: &SigningKey,
    limits: Limits,
) -> Result<Vec<TransferProof>> {
    let digest = publication.digest(limits)?;
    let proofs = stored(tx, scope, &digest)?;
    evidence(alt_provenance::verify_origin(
        publication,
        receipt,
        &proofs,
        scope,
        key.verifying_key().as_bytes(),
        limits,
    ))?;
    Ok(proofs)
}

/// Iterate in bounded DB batches. The caller holds the instance maintenance
/// barrier, so the immutable publication inventory cannot change underneath it.
fn publications(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    mut visit: impl FnMut(&mut Transaction<'_>, Scope, Publication, Receipt) -> Result<()>,
) -> Result<()> {
    let mut cursor: Option<(String, String)> = None;
    loop {
        let (repository, digest) = cursor.clone().unwrap_or_default();
        let rows = tx.query("SELECT h.repository,h.digest,r.recovery_epoch,h.publication,h.receipt FROM (SELECT repository,digest,publication,result->'receipt' AS receipt FROM alt_requests WHERE state='committed' UNION ALL SELECT repository,digest,publication,receipt FROM alt_archived_publications) h JOIN alt_repositories r ON r.id=h.repository WHERE (h.repository,h.digest)>($1,$2) ORDER BY h.repository,h.digest LIMIT 100", &[&repository,&digest])?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let repository: String = row.get(0);
            cursor = Some((repository.clone(), row.get(1)));
            let scope = Scope {
                installation_id: spec.installation_id.clone(),
                repository_instance_id: repository,
                recovery_epoch: crate::store::unsigned(row.get(2))?,
            };
            visit(
                tx,
                scope,
                serde_json::from_value(row.get(3))?,
                serde_json::from_value(row.get(4))?,
            )?;
        }
    }
    Ok(())
}

/// Schema-five recovery already retained the exact backup identity in a
/// durable audit event. Only that matching same-installation/key/epoch evidence
/// permits conversion of a historical receipt into the explicit format.
pub(crate) fn backfill_legacy(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &SigningKey,
    limits: Limits,
) -> Result<()> {
    publications(tx, spec, |tx, scope, publication, receipt| {
        let digest = publication.digest(limits)?;
        let mut proofs = stored(tx, &scope, &digest)?;
        if proofs.is_empty() && publication.key.scope != scope {
            let row = tx.query_opt("SELECT details FROM alt_instance_audit WHERE action='instance.restore' AND subject_id=$1 AND details @> $2 ORDER BY sequence DESC LIMIT 1", &[&spec.installation_id,&serde_json::json!({"generation":scope.recovery_epoch})])?
                .ok_or(Error::Integrity("historical recovery audit absent"))?;
            let details: serde_json::Value = row.get(0);
            let backup = details
                .get("backup_digest")
                .and_then(|v| v.as_str())
                .ok_or(Error::Integrity("historical recovery backup identity"))?;
            // sign_transfer enforces same installation/repository/key and a
            // strictly increasing epoch. It cannot invent a migration proof.
            let public_key = alt_provenance::encode_hex(key.verifying_key().as_bytes());
            proofs.push(evidence(alt_provenance::sign_transfer(
                TransferBody {
                    version: 1,
                    publication_digest: digest.clone(),
                    source: publication.key.scope.clone(),
                    target: scope.clone(),
                    source_public_key: public_key.clone(),
                    target_public_key: public_key,
                    operation_id: format!("legacy-restore-{}", scope.recovery_epoch),
                    previous_proof: None,
                    reason: TransferReason::Recovery {
                        backup_digest: backup.into(),
                    },
                },
                key,
            ))?);
        }
        evidence(alt_provenance::verify_origin(
            &publication,
            &receipt,
            &proofs,
            &scope,
            key.verifying_key().as_bytes(),
            limits,
        ))?;
        save(tx, &scope, &digest, &proofs)
    })
}

pub(crate) fn append_recovery(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &SigningKey,
    limits: Limits,
    generation: u64,
    backup_digest: &str,
) -> Result<()> {
    publications(tx, spec, |tx, scope, publication, receipt| {
        let mut proofs = load(tx, &scope, &publication, &receipt, key, limits)?;
        if proofs.len() >= alt_remote_protocol::MAX_TRANSFER_PROOFS {
            return Err(Error::Limit("publication transfer chain"));
        }
        let digest = publication.digest(limits)?;
        let mut target = scope.clone();
        target.recovery_epoch = generation;
        let public_key = alt_provenance::encode_hex(key.verifying_key().as_bytes());
        let previous_proof = proofs.last().map(TransferProof::digest).transpose()?;
        proofs.push(evidence(alt_provenance::sign_transfer(
            TransferBody {
                version: 1,
                publication_digest: digest.clone(),
                source: scope.clone(),
                target: target.clone(),
                source_public_key: public_key.clone(),
                target_public_key: public_key,
                operation_id: format!("restore-{generation}"),
                previous_proof,
                reason: TransferReason::Recovery {
                    backup_digest: backup_digest.into(),
                },
            },
            key,
        ))?);
        evidence(alt_provenance::verify_origin(
            &publication,
            &receipt,
            &proofs,
            &target,
            key.verifying_key().as_bytes(),
            limits,
        ))?;
        save(tx, &scope, &digest, &proofs)
    })
}
