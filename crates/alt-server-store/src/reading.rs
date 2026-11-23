//! Reads of a repository that wait for no push to it.
//!
//! A transaction here reads statement by statement, each seeing what is committed
//! when it runs. So that the state of a repository, its references and what is
//! read after them belong together, a transaction locks the row of the repository
//! first: nothing of the repository changes until it is over. A push holds that
//! lock while its objects are checked, and a read that takes it waits for the push,
//! as every other read of the repository waits for that read.
//!
//! A read needs the lock for nothing else. It asks for one view of the database
//! instead, taken as it begins: every statement of it sees the repository as it
//! was committed then, whatever is committed meanwhile, and nobody waits. What a
//! read is given is what it would have been given had it come a moment earlier.
//!
//! A Cloud instance records the clock in a row with every transaction, which such a
//! view cannot be combined with: there a read locks as before.

use crate::database::Transaction;
use crate::store::unsigned;
use crate::{Error, InstanceSpec, Result};
use alt_remote_protocol::{DeploymentMode, ObjectFormat, Scope};
use alt_repository_service::RepositorySnapshot;
use std::collections::{BTreeMap, BTreeSet};

/// Begins a read, before the first statement of its transaction.
pub(crate) fn begin(tx: &mut Transaction<'_>, spec: &InstanceSpec) -> Result<()> {
    if spec.deployment_mode == DeploymentMode::SelfHosted {
        // as a statement like the others: the plans the connection keeps
        // for them are kept
        tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", &[])?;
    }
    Ok(())
}

/// Begins a read of `repository` in a transaction that has run no statement yet:
/// the instance is checked, and the repository is given as the read sees it.
pub(crate) fn open(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &alt_crypto::ed25519::SigningKey,
    repository: &str,
) -> Result<RepositorySnapshot> {
    begin(tx, spec)?;
    crate::store::instance(tx, spec, key)?;
    snapshot(tx, spec, repository)
}

/// The repository as the read that was begun sees it.
pub(crate) fn snapshot(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    repository: &str,
) -> Result<RepositorySnapshot> {
    let Some(mut current) = unread(tx, spec, repository)? else {
        return crate::store::snapshot(tx, spec, repository);
    };
    current.refs = refs(tx, repository)?;
    Ok(current)
}

/// The references of the repository.
pub(crate) fn refs(
    tx: &mut Transaction<'_>,
    repository: &str,
) -> Result<BTreeMap<String, alt_git_codec::ObjectId>> {
    let refs = tx.query_one(crate::refs::REFS_TEXT, &[&repository])?;
    crate::refs::map(refs.get::<_, Option<&str>>(0))
}

/// The repository without its references; nothing where reads lock as before.
pub(crate) fn unread(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    repository: &str,
) -> Result<Option<RepositorySnapshot>> {
    if spec.deployment_mode != DeploymentMode::SelfHosted {
        return Ok(None);
    }
    let row=tx.query_opt("SELECT recovery_epoch,object_format,policy_version,revocation_generation,policies,writes_enabled AND portable_freeze IS NULL AND portable_import IS NULL FROM alt_repositories WHERE id=$1",&[&repository])?.ok_or(Error::Unauthorized)?;
    if row.get::<_, String>(1) != "sha1" {
        return Err(Error::Precondition("unsupported repository object format"));
    }
    Ok(Some(RepositorySnapshot {
        scope: Scope {
            installation_id: spec.installation_id.clone(),
            repository_instance_id: repository.into(),
            recovery_epoch: unsigned(row.get(0))?,
        },
        runtime_generation: spec.runtime_generation,
        revocation_generation: unsigned(row.get(3))?,
        policy_version: unsigned(row.get(2))?,
        object_format: ObjectFormat::Sha1,
        refs: BTreeMap::new(),
        policies: row.get::<_, crate::Jsonb>(4).read()?,
        approved_publications: BTreeSet::new(),
        writes_enabled: row.get(5),
    }))
}
