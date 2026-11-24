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
//! A Cloud instance holds every transaction to the clock and the authority it has
//! recorded, in one row that the others lock and write. A read looks at that row
//! as its view has it and writes nothing to it.

mod again;

use crate::database::Transaction;
use crate::store::unsigned;
use crate::{Error, InstanceSpec, Result};
use alt_remote_protocol::{DeploymentMode, ObjectFormat, Scope};
use alt_repository_service::RepositorySnapshot;
use std::collections::{BTreeMap, BTreeSet};

/// Whether `error` is that of a read whose view could not be kept: a row it takes
/// a share in, of its caller's credential or of the instance, was changed by another
/// transaction after the read began.
fn overtaken(error: &Error) -> bool {
    matches!(error, Error::Database(error) if error.code() == Some("40001"))
}

/// Whether `error` is that of a read whose bytes were moved after it began: its
/// view places them in a file that was since put together with others and
/// removed. No lock holds that off, as none holds off the read.
fn moved(error: &Error) -> bool {
    matches!(error, Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// Runs `read`, and once more where it was overtaken or its bytes were moved:
/// begun again, it sees the change. This is how a transaction under one view is
/// meant to be used; a read that fails so a second time fails with that.
pub(crate) fn twice<T>(mut read: impl FnMut() -> Result<T>) -> Result<T> {
    match read() {
        Err(error) if overtaken(&error) || moved(&error) => read(),
        other => other,
    }
}

/// Begins the transaction of a read.
pub(crate) fn transaction<'a>(
    client: &'a mut crate::database::Connection,
    spec: &InstanceSpec,
) -> Result<Transaction<'a>> {
    Ok(client.reading(spec.deployment_mode == DeploymentMode::Cloud)?)
}

/// Begins a read of `repository` in the transaction of a read that has run no
/// statement yet: the instance is checked, and the repository is given as the
/// read sees it.
pub(crate) fn open(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &alt_crypto::ed25519::SigningKey,
    repository: &str,
) -> Result<RepositorySnapshot> {
    crate::store::instance(tx, spec, key)?;
    snapshot(tx, spec, repository)
}

/// The repository as the read that was begun sees it.
pub(crate) fn snapshot(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    repository: &str,
) -> Result<RepositorySnapshot> {
    let mut current = unread(tx, spec, repository)?;
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

/// The repository without its references.
pub(crate) fn unread(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    repository: &str,
) -> Result<RepositorySnapshot> {
    let row=tx.query_opt("SELECT recovery_epoch,object_format,policy_version,revocation_generation,policies,writes_enabled AND portable_freeze IS NULL AND portable_import IS NULL FROM alt_repositories WHERE id=$1",&[&repository])?.ok_or(Error::Unauthorized)?;
    if row.get::<_, String>(1) != "sha1" {
        return Err(Error::Precondition("unsupported repository object format"));
    }
    let writes_enabled = row.get::<_, bool>(5) && crate::cloud::writes_enabled(tx, spec)?;
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
        refs: BTreeMap::new(),
        policies: row.get::<_, crate::Jsonb>(4).read()?,
        approved_publications: BTreeSet::new(),
        writes_enabled,
    })
}

/// The authority of the repository as the read that was begun sees it.
pub(crate) fn authority(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    repository: &str,
) -> Result<crate::store::RepositoryAuthority> {
    let row = tx.query_opt(
        "SELECT recovery_epoch,object_format,revocation_generation FROM alt_repositories WHERE id=$1",
        &[&repository],
    )?;
    crate::store::RepositoryAuthority::of(spec, repository, row.ok_or(Error::Unauthorized)?)
}

/// The row of a request that is being uploaded, as the read sees it and without
/// a lock on it: a lock would fail where the upload went on since the read began.
pub(crate) fn upload_request(
    tx: &mut Transaction<'_>,
    key: &alt_remote_protocol::RequestKey,
) -> Result<Option<alt_pg::Row>> {
    Ok(tx.query_opt("SELECT state,expires_at,publication,reserved_bytes FROM alt_requests WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[&key.scope.repository_instance_id,&crate::store::integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_whose_file_is_gone_is_begun_again_once_and_no_other_is() {
        let gone = || Error::Io(std::io::ErrorKind::NotFound.into());
        let mut reads = 0;
        let read = twice(|| {
            reads += 1;
            if reads == 1 { Err(gone()) } else { Ok(reads) }
        });
        assert_eq!(read.unwrap(), 2);
        let mut reads = 0;
        let read: Result<()> = twice(|| {
            reads += 1;
            Err(gone())
        });
        assert!(matches!(read, Err(Error::Io(_))));
        assert_eq!(reads, 2);
        let mut reads = 0;
        let read: Result<()> = twice(|| {
            reads += 1;
            Err(Error::Io(std::io::ErrorKind::PermissionDenied.into()))
        });
        assert!(read.is_err());
        assert_eq!(reads, 1);
    }
}
