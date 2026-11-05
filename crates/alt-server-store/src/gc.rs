use crate::publication::finish;
use crate::store::{instance, integer, now, require_admin, snapshot, unsigned};
use crate::{Credential, Error, Result, Store};
use alt_remote_protocol::{RequestKey, RequestState};

impl Store {
    /// Expire requests and reclaim unpinned bytes in bounded batches. Marking
    /// deletion commits before filesystem work; retries retain that marker.
    pub fn collect_orphans(
        &mut self,
        admin: &Credential,
        repository: &str,
        limit: u32,
    ) -> Result<usize> {
        if limit == 0 || limit > 1000 {
            return Err(Error::Limit("GC batch size"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, repository)?;
        require_admin(&mut tx, &self.spec, admin)?;
        let time = now(&mut tx)?;
        for row in tx.query("SELECT epoch,actor_id,request_id,digest,reserved_bytes FROM alt_requests WHERE repository=$1 AND epoch=$4 AND state IN ('receiving','ready') AND expires_at<=$2 ORDER BY expires_at LIMIT $3 FOR UPDATE",&[&repository,&integer(time)?,&i64::from(limit),&integer(current.scope.recovery_epoch)?])? {
            let mut scope=current.scope.clone(); scope.recovery_epoch=unsigned(row.get(0))?;
            let key=RequestKey { scope,actor_id:row.get(1),request_id:row.get(2) };
            let state=RequestState::Expired {digest:row.get(3)};
            finish(&mut tx,&key,"expired",&state,unsigned(row.get(4))?)?;
        }
        tx.execute(
            "DELETE FROM alt_pins WHERE repository=$1 AND expires_at<=$2",
            &[&repository, &integer(time)?],
        )?;
        let rows=tx.query("SELECT blake3,bytes,accounted,generation FROM alt_blobs b WHERE repository=$1 AND NOT EXISTS (SELECT 1 FROM alt_pins p WHERE p.repository=b.repository AND p.blake3=b.blake3) ORDER BY created_at,blake3 LIMIT $2",&[&repository,&i64::from(limit)])?;
        let mut deleting = Vec::new();
        for row in rows {
            let digest: String = row.get(0);
            tx.execute(
                "UPDATE alt_blobs SET state='deleting' WHERE repository=$1 AND blake3=$2",
                &[&repository, &digest],
            )?;
            deleting.push((
                digest,
                unsigned(row.get(1))?,
                row.get::<_, bool>(2),
                row.get::<_, i64>(3),
            ));
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        let mut removed = 0;
        for (digest, bytes, accounted, generation) in deleting {
            let mut tx = self.client.transaction()?;
            instance(&mut tx, &self.spec, &self.signing_key)?;
            let _ = snapshot(&mut tx, &self.spec, repository)?;
            // A previous GC worker may have completed this same marker.
            let Some(row) = tx.query_opt(
                "SELECT state,generation FROM alt_blobs WHERE repository=$1 AND blake3=$2",
                &[&repository, &digest],
            )?
            else {
                continue;
            };
            if row.get::<_, i64>(1) != generation {
                continue;
            }
            if row.get::<_, String>(0) != "deleting" {
                return Err(Error::Precondition("GC marker changed"));
            }
            self.bytes.remove(&current.scope, &digest)?;
            tx.execute(
                "DELETE FROM alt_objects WHERE repository=$1 AND blake3=$2",
                &[&repository, &digest],
            )?;
            tx.execute(
                "DELETE FROM alt_blobs WHERE repository=$1 AND blake3=$2",
                &[&repository, &digest],
            )?;
            if accounted {
                tx.execute(
                    "UPDATE alt_repositories SET stored_bytes=stored_bytes-$2 WHERE id=$1",
                    &[&repository, &integer(bytes)?],
                )?;
            }
            tx.commit().map_err(|_| Error::OutcomeUnknown)?;
            removed += 1;
        }
        Ok(removed)
    }
}
