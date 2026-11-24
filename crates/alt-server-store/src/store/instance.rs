//! The check every transaction begins with: that the database is that of this instance.

use super::*;

pub(crate) fn instance(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &SigningKey,
) -> Result<()> {
    let row=tx.query_opt("SELECT installation_id,workspace_id,deployment_mode,runtime_generation,signing_key_id FROM alt_instance WHERE singleton FOR SHARE",&[])?.ok_or(Error::Precondition("instance not initialized"))?;
    same_instance(&row, spec, key)?;
    if spec.deployment_mode == DeploymentMode::Cloud {
        crate::cloud::check_instance(tx, spec)?;
    }
    Ok(())
}
/// The identity check alone, without holding the instance row: it neither waits for
/// a backup or an upgrade nor gives the transaction anything to flush. Only for
/// work that changes no row and that a backup does not rely on standing still. A
/// Cloud instance has no such check, because its check records the clock.
pub(crate) fn instance_unlocked(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &SigningKey,
) -> Result<()> {
    let row=tx.query_opt("SELECT installation_id,workspace_id,deployment_mode,runtime_generation,signing_key_id FROM alt_instance WHERE singleton",&[])?.ok_or(Error::Precondition("instance not initialized"))?;
    same_instance(&row, spec, key)
}
fn same_instance(row: &alt_pg::Row, spec: &InstanceSpec, key: &SigningKey) -> Result<()> {
    if row.get::<_, String>(0) != spec.installation_id
        || row.get::<_, String>(1) != spec.workspace_id
        || row.get::<_, String>(2) != spec.mode()
        || unsigned(row.get(3))? != spec.runtime_generation
        || row.get::<_, String>(4)
            != alt_hash::blake3::hash(key.verifying_key().as_bytes())
                .to_hex()
                .as_str()
    {
        return Err(Error::Precondition(
            "instance identity, mode, generation or signing key",
        ));
    }
    Ok(())
}
pub(super) fn mode(mode: DeploymentMode) -> &'static str {
    match mode {
        DeploymentMode::SelfHosted => "self_hosted",
        DeploymentMode::Cloud => "cloud",
    }
}
