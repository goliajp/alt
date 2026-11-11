mod references;

use crate::requests::pin_many;
use crate::store::{authorize, instance, integer, now, repository_authority, snapshot, unsigned};
use crate::{Credential, Error, Result, Store};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_remote_protocol::Scope;
use alt_repository_service::{ObjectReader, ObjectSource};
use std::collections::{BTreeMap, BTreeSet};

/// Snapshot metadata and protected immutable bytes. IDs identify this read;
/// they grant no authority without the original credential and scope.
pub struct ReadSnapshot {
    pub scope: Scope,
    pub refs: BTreeMap<String, ObjectId>,
    pub default_branch: String,
    pub transaction_sequence: u64,
    pin_id: String,
    actor: String,
    expires_at: u64,
    objects: BTreeMap<ObjectId, (ObjectKind, String, u64)>,
    locations: crate::segments::Locations,
}
/// Local input for a buffered transformation. This reader does not authorize
/// external output: `with_snapshot_objects` checks current authority and pins
/// again before returning the transformation's result.
pub struct SnapshotObjects<'a> {
    bytes: crate::bytes::ReadSession,
    read: &'a ReadSnapshot,
}
impl ObjectSource for SnapshotObjects<'_> {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        let Some((kind, digest, bytes)) = self.read.objects.get(&oid) else {
            return Ok(None);
        };
        let file = self
            .bytes
            .read(digest, *bytes, self.read.locations.get(digest))
            .map_err(|error| match error {
                Error::Io(error) => alt_repository_service::Error::Io(error),
                _ => alt_repository_service::Error::Integrity("pinned read bytes unavailable"),
            })?;
        Ok(Some(ObjectReader {
            kind: *kind,
            bytes: *bytes,
            reader: Box::new(file),
        }))
    }
}
impl ReadSnapshot {
    pub fn objects(&self) -> impl Iterator<Item = (ObjectId, ObjectKind, u64)> + '_ {
        self.objects
            .iter()
            .map(|(id, (kind, _, bytes))| (*id, *kind, *bytes))
    }
}
#[derive(Clone, Copy)]
enum Selection<'a> {
    All(usize),
    Metadata,
    RefTips(&'a [String]),
}

impl Store {
    /// Pin the currently published repository snapshot for fetch/export/backup.
    /// This is also the metadata watermark used by portable snapshot writers.
    pub fn read_snapshot(
        &mut self,
        credential: &Credential,
        repository: &str,
        ttl_ms: u64,
        max_objects: usize,
    ) -> Result<ReadSnapshot> {
        if max_objects == 0 {
            return Err(Error::Limit("read pin lifetime or object bound"));
        }
        self.read_snapshot_inner(
            credential,
            repository,
            ttl_ms,
            Selection::All(max_objects.min(self.limits.graph_objects)),
        )
    }
    /// Snapshot current refs and repository metadata without selecting or
    /// pinning contents. It exposes no object readers and cannot serve as a
    /// fetch/export snapshot. Buffered metadata responses must still use
    /// `with_snapshot_objects` for final authority, scope and expiry checks.
    pub fn read_metadata_snapshot(
        &mut self,
        credential: &Credential,
        repository: &str,
        ttl_ms: u64,
    ) -> Result<ReadSnapshot> {
        self.read_snapshot_inner(credential, repository, ttl_ms, Selection::Metadata)
    }
    fn read_snapshot_inner(
        &mut self,
        credential: &Credential,
        repository: &str,
        ttl_ms: u64,
        selection: Selection<'_>,
    ) -> Result<ReadSnapshot> {
        if ttl_ms == 0 || ttl_ms > 3_600_000 {
            return Err(Error::Limit("read pin lifetime or object bound"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let expires = now(&mut tx)?
            .checked_add(ttl_ms)
            .ok_or(Error::Limit("read expiry"))?;
        let mut nonce = [0u8; 16];
        alt_os::random::fill(&mut nonce);
        let pin_id = format!(
            "read:{}",
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let metadata = tx.query_one(
            "SELECT transaction_sequence,default_branch FROM alt_repositories WHERE id=$1",
            &[&repository],
        )?;
        let sequence = unsigned(metadata.get(0))?;
        let default_branch: String = metadata.get(1);
        alt_remote_protocol::validate_ref_name(&default_branch)?;
        if !default_branch.starts_with("refs/heads/") {
            return Err(Error::Integrity("repository default branch"));
        }
        let mut objects = BTreeMap::new();
        // Fresh repositories may not yet have planner statistics. Keep the
        // exact intersection bounded instead of allowing a permanent-pin
        // semijoin to compare every object against every publication root.
        // The repository lock still covers all pages and the new read pins.
        let mut after = String::new();
        if let Selection::All(max_objects) = selection {
            loop {
                let rows = tx.query(
                    "SELECT oid,kind,blake3 FROM alt_objects WHERE repository=$1 AND oid>$2 ORDER BY oid LIMIT $3",
                    &[&repository, &after, &integer(crate::database::BATCH_ROWS as u64)?],
                )?;
                if rows.is_empty() {
                    break;
                }
                let digests: Vec<&str> = rows.iter().map(|row| row.get(2)).collect();
                let stored: BTreeMap<String, i64> = tx.query(
                    "SELECT blake3,bytes FROM alt_blobs WHERE repository=$1 AND state='stored' AND blake3=ANY($2)",
                    &[&repository, &digests],
                )?.into_iter().map(|row| (row.get(0), row.get(1))).collect();
                // Project membership for each requested key. With fresh table
                // statistics, a generic ANY filter or join can compare every
                // permanent pin against every requested digest.
                let pinned: BTreeSet<String> = tx.query(
                    "SELECT wanted.blake3,EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=$1 AND p.expires_at IS NULL AND p.blake3=wanted.blake3) FROM unnest($2::text[]) AS wanted(blake3)",
                    &[&repository, &digests],
                )?.into_iter().filter(|row| row.get::<_, bool>(1)).map(|row| row.get(0)).collect();
                for row in rows {
                    let oid: String = row.get(0);
                    let kind: String = row.get(1);
                    let digest: String = row.get(2);
                    after.clone_from(&oid);
                    let Some(bytes) = stored.get(&digest).filter(|_| pinned.contains(&digest))
                    else {
                        continue;
                    };
                    objects.insert(
                        oid.parse()
                            .map_err(|_| Error::Integrity("read object id"))?,
                        (
                            kind.parse()
                                .map_err(|_| Error::Integrity("read object kind"))?,
                            digest,
                            unsigned(*bytes)?,
                        ),
                    );
                    if objects.len() > max_objects {
                        return Err(Error::Limit("read graph objects"));
                    }
                }
            }
        }
        if let Selection::RefTips(prefixes) = selection {
            let matches =
                |name: &str| prefixes.is_empty() || prefixes.iter().any(|p| name.starts_with(p));
            let tips: BTreeSet<_> = current
                .refs
                .iter()
                .filter(|(name, _)| {
                    matches(name) || (name.as_str() == default_branch && matches("HEAD"))
                })
                .map(|(_, oid)| oid.to_string())
                .collect();
            if tips.len() > self.limits.graph_objects {
                return Err(Error::Limit("reference snapshot objects"));
            }
            if tips.len() > self.limits.walk_steps {
                return Err(Error::Limit("reference tag walk"));
            }
            objects = references::select_objects(
                &mut tx,
                repository,
                &tips.into_iter().collect::<Vec<_>>(),
            )?;
        }
        let digests: Vec<_> = objects
            .values()
            .map(|(_, digest, _)| digest.as_str())
            .collect();
        pin_many(&mut tx, repository, &pin_id, &digests, Some(expires))?;
        let locations = crate::segments::locations(&mut tx, repository, &digests)?;
        if matches!(selection, Selection::RefTips(_)) {
            // Ref selection and its initial pins share this repository barrier.
            // Recheck time after all batches, as in later tag expansions.
            instance(&mut tx, &self.spec, &self.signing_key)?;
            let current_auth = authorize(&mut tx, &self.spec, credential, &current)?;
            let time = now(&mut tx)?;
            if expires <= time || current_auth.expires_at <= time {
                return Err(Error::Precondition("read pin or credential expired"));
            }
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(ReadSnapshot {
            scope: current.scope,
            refs: current.refs,
            default_branch,
            transaction_sequence: sequence,
            pin_id,
            actor: auth.actor_id,
            expires_at: expires,
            objects,
            locations,
        })
    }
    /// Compute a complete local result without holding database locks across
    /// byte reads or CPU work. The callback must buffer its result and must not
    /// stream bytes or perform other external side effects. A failed final
    /// check drops the result. Cloud callers refresh platform authority here,
    /// before the final transaction; self-hosted callers may reuse the token.
    pub fn with_snapshot_objects<T>(
        &mut self,
        credential: &Credential,
        read: &ReadSnapshot,
        work: impl FnOnce(&SnapshotObjects<'_>) -> Result<T>,
        refresh: impl FnOnce(&mut Store) -> Result<Credential>,
    ) -> Result<T> {
        self.check_read_snapshot(credential, read)?;
        let objects = SnapshotObjects {
            bytes: crate::bytes::ReadSession::new(&self.bytes, &read.scope),
            read,
        };
        let result = work(&objects)?;
        objects.bytes.finish()?;
        let current = refresh(self)?;
        self.check_read_snapshot(&current, read)?;
        Ok(result)
    }
    fn check_read_snapshot(&mut self, credential: &Credential, read: &ReadSnapshot) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let repository = &read.scope.repository_instance_id;
        let current = repository_authority(&mut tx, &self.spec, repository)?;
        let auth = current.authorize(&mut tx, &self.spec, credential)?;
        if current.scope != read.scope || auth.actor_id != read.actor {
            return Err(Error::Unauthorized);
        }
        let time = now(&mut tx)?;
        if read.expires_at <= time {
            return Err(Error::Precondition("read pin expired"));
        }
        let mut expires = read.expires_at;
        let objects: Vec<_> = read.objects.values().collect();
        for batch in objects.chunks(crate::database::BATCH_ROWS) {
            let digests: Vec<_> = batch.iter().map(|(_, digest, _)| digest.as_str()).collect();
            let stored: BTreeMap<String, i64> = tx
                .query(
                    "SELECT blake3,bytes FROM alt_blobs WHERE repository=$1 AND state='stored' AND blake3=ANY($2)",
                    &[repository, &digests],
                )?
                .into_iter()
                .map(|row| (row.get(0), row.get(1)))
                .collect();
            let pinned: BTreeMap<String, i64> = tx
                .query(
                    "SELECT blake3,expires_at FROM alt_pins WHERE repository=$1 AND pin_id=$2 AND expires_at>$3 AND blake3=ANY($4)",
                    &[repository, &read.pin_id, &integer(time)?, &digests],
                )?
                .into_iter()
                .map(|row| (row.get(0), row.get(1)))
                .collect();
            for (_, digest, bytes) in batch {
                if stored.get(digest) != Some(&integer(*bytes)?) || !pinned.contains_key(digest) {
                    return Err(Error::Precondition(
                        "read pin expired or object unavailable",
                    ));
                }
                expires = expires.min(unsigned(pinned[digest])?);
            }
        }
        // Locks prevent revocation during these queries, but cannot stop time.
        // Recheck leases after all batches, before returning a buffered result.
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let auth = current.authorize(&mut tx, &self.spec, credential)?;
        let time = now(&mut tx)?;
        if expires <= time || auth.expires_at <= time {
            return Err(Error::Precondition("read pin or credential expired"));
        }
        tx.commit()?;
        Ok(())
    }
    pub fn open_snapshot_object(
        &mut self,
        credential: &Credential,
        read: &ReadSnapshot,
        oid: ObjectId,
    ) -> Result<(ObjectKind, u64, crate::StoredReader)> {
        let (kind, digest, bytes) = read.objects.get(&oid).ok_or(Error::Unauthorized)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current =
            repository_authority(&mut tx, &self.spec, &read.scope.repository_instance_id)?;
        let auth = current.authorize(&mut tx, &self.spec, credential)?;
        if current.scope != read.scope || auth.actor_id != read.actor {
            return Err(Error::Unauthorized);
        }
        let time = now(&mut tx)?;
        if tx.query_opt("SELECT 1 FROM alt_pins p JOIN alt_blobs b ON b.repository=p.repository AND b.blake3=p.blake3 WHERE p.repository=$1 AND p.pin_id=$2 AND p.blake3=$3 AND p.expires_at>$4 AND b.state='stored'",&[&read.scope.repository_instance_id,&read.pin_id,digest,&integer(time)?])?.is_none() {return Err(Error::Precondition("read pin expired"));}
        // Open while holding the repository lock; even a later unlink leaves
        // this descriptor valid. Every subsequent object open rechecks access.
        let file =
            self.bytes
                .read_payload(&read.scope, digest, *bytes, read.locations.get(digest))?;
        tx.commit()?;
        Ok((*kind, *bytes, file))
    }
    pub fn release_snapshot(&mut self, credential: &Credential, read: ReadSnapshot) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current =
            repository_authority(&mut tx, &self.spec, &read.scope.repository_instance_id)?;
        let auth = current.authorize(&mut tx, &self.spec, credential)?;
        if current.scope != read.scope || auth.actor_id != read.actor {
            return Err(Error::Unauthorized);
        }
        tx.execute(
            "DELETE FROM alt_pins WHERE repository=$1 AND pin_id=$2",
            &[&read.scope.repository_instance_id, &read.pin_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
