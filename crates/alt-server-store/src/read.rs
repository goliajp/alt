use crate::requests::pin;
use crate::store::{authorize, instance, integer, now, snapshot, unsigned};
use crate::{Credential, Error, Result, Store};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_remote_protocol::Scope;
use rand_core::{OsRng, RngCore};
use std::collections::BTreeMap;

/// Snapshot metadata and protected immutable bytes. IDs identify this read;
/// they grant no authority without the original credential and scope.
pub struct ReadSnapshot {
    pub scope: Scope,
    pub refs: BTreeMap<String, ObjectId>,
    pub default_branch: String,
    pub transaction_sequence: u64,
    pin_id: String,
    actor: String,
    objects: BTreeMap<ObjectId, (ObjectKind, String, u64)>,
}
impl ReadSnapshot {
    pub fn objects(&self) -> impl Iterator<Item = (ObjectId, ObjectKind, u64)> + '_ {
        self.objects
            .iter()
            .map(|(id, (kind, _, bytes))| (*id, *kind, *bytes))
    }
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
        if ttl_ms == 0 || ttl_ms > 3_600_000 || max_objects == 0 {
            return Err(Error::Limit("read pin lifetime or object bound"));
        }
        let max_objects = max_objects.min(self.limits.graph_objects);
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let expires = now(&mut tx)?
            .checked_add(ttl_ms)
            .ok_or(Error::Limit("read expiry"))?;
        let mut nonce = [0u8; 16];
        OsRng.fill_bytes(&mut nonce);
        let pin_id = format!(
            "read:{}",
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let rows=tx.query("SELECT o.oid,o.kind,o.blake3,b.bytes FROM alt_objects o JOIN alt_blobs b ON b.repository=o.repository AND b.blake3=o.blake3 WHERE o.repository=$1 AND b.state='stored' AND EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=o.repository AND p.blake3=o.blake3 AND p.expires_at IS NULL) ORDER BY o.oid LIMIT $2",&[&repository,&integer((max_objects as u64).saturating_add(1))?])?;
        if rows.len() > max_objects {
            return Err(Error::Limit("read graph objects"));
        }
        let mut objects = BTreeMap::new();
        for row in rows {
            let oid: String = row.get(0);
            let kind: String = row.get(1);
            let digest: String = row.get(2);
            let bytes = unsigned(row.get(3))?;
            pin(&mut tx, repository, &pin_id, &digest, Some(expires))?;
            objects.insert(
                oid.parse()
                    .map_err(|_| Error::Integrity("read object id"))?,
                (
                    kind.parse()
                        .map_err(|_| Error::Integrity("read object kind"))?,
                    digest,
                    bytes,
                ),
            );
        }
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
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(ReadSnapshot {
            scope: current.scope,
            refs: current.refs,
            default_branch,
            transaction_sequence: sequence,
            pin_id,
            actor: auth.actor_id,
            objects,
        })
    }
    pub fn open_snapshot_object(
        &mut self,
        credential: &Credential,
        read: &ReadSnapshot,
        oid: ObjectId,
    ) -> Result<(ObjectKind, u64, std::fs::File)> {
        let (kind, digest, bytes) = read.objects.get(&oid).ok_or(Error::Unauthorized)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &read.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if current.scope != read.scope || auth.actor_id != read.actor {
            return Err(Error::Unauthorized);
        }
        let time = now(&mut tx)?;
        if tx.query_opt("SELECT 1 FROM alt_pins p JOIN alt_blobs b ON b.repository=p.repository AND b.blake3=p.blake3 WHERE p.repository=$1 AND p.pin_id=$2 AND p.blake3=$3 AND p.expires_at>$4 AND b.state='stored'",&[&read.scope.repository_instance_id,&read.pin_id,digest,&integer(time)?])?.is_none() {return Err(Error::Precondition("read pin expired"));}
        // Open while holding the repository lock; even a later unlink leaves
        // this descriptor valid. Every subsequent object open rechecks access.
        let file = self.bytes.read(&read.scope, digest, *bytes)?;
        tx.commit()?;
        Ok((*kind, *bytes, file))
    }
    pub fn release_snapshot(&mut self, credential: &Credential, read: ReadSnapshot) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &read.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
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
