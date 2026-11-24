//! The binding a restored instance is given in place of the one its backup was taken under.

use super::*;

/// Fresh platform authority for an externally fenced disaster recovery. Content
/// and membership history survive; runtime credentials and leases do not.
pub struct CloudRestoreBinding<'a> {
    pub audience: &'a Audience,
    pub runtime_key_digest: &'a str,
    pub provision_digest: Option<&'a str>,
}
pub(crate) fn restore_binding(
    tx: &mut Transaction<'_>,
    source: &InstanceSpec,
    generation: u64,
    replacement: &CloudRestoreBinding<'_>,
) -> Result<()> {
    let row = binding(tx)?;
    let mut expected: Audience = row.get::<_, Jsonb>("audience").read_serde()?;
    audience_matches(source, &expected)?;
    expected.generation = golia_platform_contract::Counter(generation);
    let mut target = source.clone();
    target.runtime_generation = generation;
    audience_matches(&target, replacement.audience)?;
    crate::bytes::check_digest(replacement.runtime_key_digest)?;
    if let Some(digest) = replacement.provision_digest {
        crate::bytes::check_digest(digest)?;
    }
    let old_provision: Option<String> = row.get("provision_digest");
    if replacement.audience != &expected
        || replacement.runtime_key_digest == row.get::<_, String>("runtime_key_digest")
        || old_provision
            .is_some_and(|old| replacement.provision_digest.is_none_or(|new| new == old))
    {
        return Err(Error::Precondition(
            "Cloud recovery requires same identity and fresh runtime binding",
        ));
    }
    tx.execute(
        "UPDATE alt_cloud_binding SET audience=$1,runtime_key_digest=$2,provision_digest=$3,service=NULL,service_denied_at=0 WHERE singleton",
        &[&Jsonb::from_serde(&replacement.audience)?, &replacement.runtime_key_digest, &replacement.provision_digest],
    )?;
    Ok(())
}
