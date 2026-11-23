//! The authority of a repository: its epoch and revocation generation, without
//! its references.

use crate::database::Transaction;
use crate::store::unsigned;
use crate::{Credential, Error, InstanceSpec, Result};
use alt_remote_protocol::Scope;
use alt_repository_service::Authorization;

/// Current repository authority, without a ref or publication-policy view.
/// It cannot be passed to domain publication validation as a partial snapshot.
pub(crate) struct RepositoryAuthority {
    pub(crate) scope: Scope,
    revocation_generation: u64,
}
impl RepositoryAuthority {
    /// from the epoch, object format and revocation generation of the repository's row
    pub(crate) fn of(spec: &InstanceSpec, repository: &str, row: alt_pg::Row) -> Result<Self> {
        if row.get::<_, String>(1) != "sha1" {
            return Err(Error::Precondition("unsupported repository object format"));
        }
        Ok(Self {
            scope: Scope {
                installation_id: spec.installation_id.clone(),
                repository_instance_id: repository.into(),
                recovery_epoch: unsigned(row.get(0))?,
            },
            revocation_generation: unsigned(row.get(2))?,
        })
    }
    pub(crate) fn authorize(
        &self,
        tx: &mut Transaction<'_>,
        spec: &InstanceSpec,
        credential: &Credential,
    ) -> Result<Authorization> {
        crate::store::authorize_scope(
            tx,
            spec,
            credential,
            &self.scope,
            self.revocation_generation,
        )
    }
}
pub(crate) fn repository_authority(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    repository: &str,
) -> Result<RepositoryAuthority> {
    let row = tx.query_opt(
        "SELECT recovery_epoch,object_format,revocation_generation FROM alt_repositories WHERE id=$1 FOR UPDATE",
        &[&repository],
    )?.ok_or(Error::Unauthorized)?;
    RepositoryAuthority::of(spec, repository, row)
}
