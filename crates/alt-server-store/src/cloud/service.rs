//! What the service the instance is paid for allows: writes, and how many repositories.

use super::*;

/// Account authorization and service authorization are deliberately separate.
/// Missing/expired paid service disables new content without blocking exports.
pub(crate) fn service(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
) -> Result<Option<AltServiceAuthorization>> {
    if spec.deployment_mode != DeploymentMode::Cloud {
        return Ok(None);
    }
    let row = binding(tx)?;
    let value: Jsonb = row
        .get::<_, Option<Jsonb>>("service")
        .ok_or(Error::Precondition(
            "Cloud service authorization unavailable",
        ))?;
    let service: AltServiceAuthorization = value.read_serde()?;
    let audience: Audience = row.get::<_, Jsonb>("audience").read_serde()?;
    denied(service.check(&audience, now(tx)?))?;
    if service.lease.revocation_generation.0 < unsigned(row.get("authority_revision"))?
        || service.lease.issued_at_unix_ms.0 <= unsigned(row.get("service_denied_at"))?
        || !service.writes_enabled
    {
        return Err(Error::Precondition(
            "Cloud writes are not currently authorized",
        ));
    }
    Ok(Some(service))
}
pub(crate) fn writes_enabled(tx: &mut Transaction<'_>, spec: &InstanceSpec) -> Result<bool> {
    if spec.deployment_mode != DeploymentMode::Cloud {
        return Ok(true);
    }
    match service(tx, spec) {
        Ok(_) => Ok(true),
        Err(Error::Precondition(_) | Error::Unauthorized) => Ok(false),
        Err(error) => Err(error),
    }
}
pub(crate) fn repository_quota(tx: &mut Transaction<'_>, spec: &InstanceSpec) -> Result<()> {
    if let Some(service) = service(tx, spec)? {
        let count: i64 = tx
            .query_one("SELECT count(*) FROM alt_repositories", &[])?
            .get(0);
        if unsigned(count)? >= service.limits.repositories.0 {
            return Err(Error::Limit("Cloud repository quota"));
        }
    }
    Ok(())
}
