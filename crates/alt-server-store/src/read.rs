mod references;

use crate::store::{authorize, instance, now, repository_authority, snapshot, unsigned};
use crate::{Credential, Error, Result, Store};
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_pack_segment::SegmentSet;
use alt_remote_protocol::Scope;
use alt_repository_service::{ObjectReader, ObjectSource};
use std::collections::{BTreeMap, BTreeSet};

/// Snapshot metadata and the selected objects of the pack segments that were live
/// when it was taken. IDs identify this read; they grant no authority without the
/// original credential and scope.
pub struct ReadSnapshot {
    pub scope: Scope,
    pub refs: BTreeMap<String, ObjectId>,
    pub default_branch: String,
    pub transaction_sequence: u64,
    actor: String,
    expires_at: u64,
    objects: BTreeMap<ObjectId, (ObjectKind, u64)>,
    /// segment files stay mapped while this set lives, also after a merge retires them
    segments: SegmentSet,
}
/// Local input for a buffered transformation. This reader does not authorize
/// external output: `with_snapshot_objects` checks current authority again
/// before returning the transformation's result.
pub struct SnapshotObjects<'a> {
    read: &'a ReadSnapshot,
}
impl ObjectSource for SnapshotObjects<'_> {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        let Some((kind, bytes)) = self.read.objects.get(&oid) else {
            return Ok(None);
        };
        let data = self.read.bytes(oid).map_err(|_| {
            alt_repository_service::Error::Integrity("snapshot object bytes unavailable")
        })?;
        Ok(Some(ObjectReader {
            kind: *kind,
            bytes: *bytes,
            reader: Box::new(std::io::Cursor::new(data)),
        }))
    }
}
fn segment_key(oid: ObjectId) -> Result<[u8; 20]> {
    oid.as_bytes()
        .try_into()
        .map_err(|_| Error::Integrity("read object id"))
}
impl ReadSnapshot {
    pub fn objects(&self) -> impl Iterator<Item = (ObjectId, ObjectKind, u64)> + '_ {
        self.objects
            .iter()
            .map(|(id, (kind, bytes))| (*id, *kind, *bytes))
    }
    /// kind and size of an object the snapshot's segments hold
    fn locate(&self, oid: ObjectId) -> Result<Option<(ObjectKind, u64)>> {
        Ok(self
            .segments
            .record(&segment_key(oid)?)
            .map_err(|_| Error::Integrity("pack segment record"))?
            .map(|record| (crate::packs::object_kind(record.kind), record.size)))
    }
    fn bytes(&self, oid: ObjectId) -> Result<Vec<u8>> {
        let (_, data) = self
            .segments
            .read(&segment_key(oid)?)
            .map_err(|_| Error::Integrity("snapshot object bytes"))?;
        Ok(data)
    }
    fn select_all(&mut self, max_objects: usize) -> Result<()> {
        for segment in self.segments.segments() {
            for index in 0..segment.len() {
                let record = segment
                    .record(index)
                    .map_err(|_| Error::Integrity("pack segment record"))?;
                self.objects.insert(
                    ObjectId::from_bytes(HashAlgo::Sha1, &record.oid)
                        .map_err(|_| Error::Integrity("read object id"))?,
                    (crate::packs::object_kind(record.kind), record.size),
                );
                if self.objects.len() > max_objects {
                    return Err(Error::Limit("read graph objects"));
                }
            }
        }
        Ok(())
    }
    /// adds objects that must be present: ref tips and the targets of selected tags
    fn select(&mut self, wanted: impl IntoIterator<Item = ObjectId>) -> Result<()> {
        for oid in wanted {
            let found = self
                .locate(oid)?
                .ok_or(Error::Integrity("published reference target unavailable"))?;
            self.objects.insert(oid, found);
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum Selection<'a> {
    All(usize),
    Metadata,
    RefTips(&'a [String]),
}

impl Store {
    /// The currently published repository snapshot for fetch/export/backup:
    /// every object its segments hold, whichever protocol brought it in.
    pub fn read_snapshot(
        &mut self,
        credential: &Credential,
        repository: &str,
        ttl_ms: u64,
        max_objects: usize,
    ) -> Result<ReadSnapshot> {
        if max_objects == 0 {
            return Err(Error::Limit("read snapshot lifetime or object bound"));
        }
        self.read_snapshot_inner(
            credential,
            repository,
            ttl_ms,
            Selection::All(max_objects.min(self.limits.graph_objects)),
        )
    }
    /// Snapshot current refs and repository metadata without selecting
    /// contents. It exposes no object readers and cannot serve as a
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
            return Err(Error::Limit("read snapshot lifetime or object bound"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let expires = now(&mut tx)?
            .checked_add(ttl_ms)
            .ok_or(Error::Limit("read expiry"))?;
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
        // refs and the segment list come from the same transaction
        let ids = Self::live_segment_ids(&mut tx, repository)?;
        tx.commit()?;
        let mut read = ReadSnapshot {
            scope: current.scope,
            refs: current.refs,
            default_branch,
            transaction_sequence: sequence,
            actor: auth.actor_id,
            expires_at: expires,
            objects: BTreeMap::new(),
            segments: match selection {
                Selection::Metadata => SegmentSet::default(),
                _ => self.open_listed_segments(repository, ids)?.1,
            },
        };
        match selection {
            Selection::All(max_objects) => read.select_all(max_objects)?,
            Selection::Metadata => {}
            Selection::RefTips(prefixes) => {
                let matches = |name: &str| {
                    prefixes.is_empty() || prefixes.iter().any(|p| name.starts_with(p))
                };
                let tips: BTreeSet<_> = read
                    .refs
                    .iter()
                    .filter(|(name, _)| {
                        matches(name) || (**name == read.default_branch && matches("HEAD"))
                    })
                    .map(|(_, oid)| *oid)
                    .collect();
                if tips.len() > self.limits.graph_objects {
                    return Err(Error::Limit("reference snapshot objects"));
                }
                if tips.len() > self.limits.walk_steps {
                    return Err(Error::Limit("reference tag walk"));
                }
                read.select(tips)?;
            }
        }
        Ok(read)
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
        let result = work(&SnapshotObjects { read })?;
        let current = refresh(self)?;
        self.check_read_snapshot(&current, read)?;
        Ok(result)
    }
    /// The snapshot is still this actor's to read: same repository epoch, current
    /// grants, and neither the snapshot nor the credential has run out.
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
            return Err(Error::Precondition("read snapshot expired"));
        }
        if auth.expires_at <= time {
            return Err(Error::Precondition("read snapshot or credential expired"));
        }
        tx.commit()?;
        Ok(())
    }
    /// One selected object. Every open rechecks current access.
    pub fn open_snapshot_object(
        &mut self,
        credential: &Credential,
        read: &ReadSnapshot,
        oid: ObjectId,
    ) -> Result<(ObjectKind, u64, std::io::Cursor<Vec<u8>>)> {
        let (kind, bytes) = read.objects.get(&oid).ok_or(Error::Unauthorized)?;
        self.check_read_snapshot(credential, read)?;
        Ok((*kind, *bytes, std::io::Cursor::new(read.bytes(oid)?)))
    }
}
