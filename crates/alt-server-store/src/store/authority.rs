//! Whether a credential may do what a request asks of a repository, and the repository as a transaction that has locked it sees it.

use super::*;

pub(crate) fn credential_row(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
) -> Result<alt_pg::Row> {
    let row=tx.query_opt("SELECT actor_id,attribution,identity_source,expires_at,revoked,administrator FROM alt_credentials WHERE token_digest=$1 FOR SHARE",&[&credential.digest])?.ok_or(Error::Unauthorized)?;
    if row.get::<_, String>(2) != spec.mode()
        || row.get::<_, bool>(4)
        || unsigned(row.get(3))? <= now(tx)?
    {
        return Err(Error::Unauthorized);
    }
    let delegated = crate::delegation::check(tx, spec, credential, &row)?;
    if delegated {
        crate::telemetry::authenticated(&row);
        return Ok(row);
    }
    if spec.deployment_mode == DeploymentMode::Cloud {
        crate::cloud::check_credential(tx, spec, credential, &row)?;
    } else {
        crate::browser::check_credential(tx, credential, &row.get::<_, String>("actor_id"))?;
    }
    crate::telemetry::authenticated(&row);
    Ok(row)
}
pub(crate) fn require_admin(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
) -> Result<()> {
    if !credential_row(tx, spec, credential)?.get::<_, bool>(5) {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
pub(crate) fn authorize(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
    snapshot: &RepositorySnapshot,
) -> Result<Authorization> {
    authorize_scope(
        tx,
        spec,
        credential,
        &snapshot.scope,
        snapshot.revocation_generation,
    )
}

pub(crate) fn authorize_scope(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
    scope: &Scope,
    revocation_generation: u64,
) -> Result<Authorization> {
    let row = credential_row(tx, spec, credential)?;
    let actor: String = row.get(0);
    let grant=tx.query_opt("SELECT can_read,publish_records,grants FROM alt_grants WHERE actor_id=$1 AND repository=$2",&[&actor,&scope.repository_instance_id])?.ok_or(Error::Unauthorized)?;
    // Administrators do not get implicit access to repository contents.
    if !grant.get::<_, bool>(0) {
        return Err(Error::Unauthorized);
    }
    let mut auth = Authorization {
        scope: scope.clone(),
        runtime_generation: spec.runtime_generation,
        actor_id: actor,
        attribution: row.get::<_, Jsonb>(1).read()?,
        expires_at: unsigned(row.get(3))?,
        revocation_generation,
        grants: grant.get::<_, Jsonb>(2).read()?,
        publish_records: grant.get(1),
    };
    crate::delegation::restrict(tx, credential, &mut auth)?;
    Ok(auth)
}
pub(crate) fn snapshot(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    repository: &str,
) -> Result<RepositorySnapshot> {
    let row=tx.query_opt("SELECT recovery_epoch,object_format,policy_version,revocation_generation,policies,writes_enabled AND portable_freeze IS NULL AND portable_import IS NULL FROM alt_repositories WHERE id=$1 FOR UPDATE",&[&repository])?.ok_or(Error::Unauthorized)?;
    if row.get::<_, String>(1) != "sha1" {
        return Err(Error::Precondition("unsupported repository object format"));
    }
    let refs = crate::refs::map(
        tx.query_one(crate::refs::REFS_TEXT, &[&repository])?
            .get::<_, Option<&str>>(0),
    )?;
    Ok(RepositorySnapshot {
        scope: Scope {
            installation_id: spec.installation_id.clone(),
            repository_instance_id: repository.into(),
            recovery_epoch: unsigned(row.get(0))?,
        },
        runtime_generation: spec.runtime_generation,
        revocation_generation: unsigned(row.get(3))?,
        policy_version: unsigned(row.get(2))?,
        object_format: ObjectFormat::Sha1,
        refs,
        policies: row.get::<_, Jsonb>(4).read()?,
        approved_publications: BTreeSet::new(),
        writes_enabled: row.get::<_, bool>(5) && crate::cloud::writes_enabled(tx, spec)?,
    })
}
