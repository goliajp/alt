//! Leasing a request to one worker, with the content it selected.
use super::*;
use crate::Jsonb;

impl Store {
    pub fn acquire_publication(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        lease_ms: u64,
        max_objects: usize,
    ) -> Result<PublicationWork> {
        self.acquire_work(credential, key, lease_ms, max_objects, false)
    }
    /// A current reviewer acts on an already-ready selected publication.
    /// This handle cannot publish on behalf of the originating actor.
    pub fn acquire_review(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        lease_ms: u64,
        max_objects: usize,
    ) -> Result<PublicationWork> {
        self.acquire_work(credential, key, lease_ms, max_objects, true)
    }
    fn acquire_work(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        lease_ms: u64,
        max_objects: usize,
        review: bool,
    ) -> Result<PublicationWork> {
        if lease_ms == 0 || lease_ms > 300_000 || max_objects == 0 {
            return Err(Error::Limit("worker lease or object bound"));
        }
        let max_objects = max_objects.min(self.limits.graph_objects);
        // Selected Git objects that were published earlier are confirmed against the
        // segments and the refs as they are before the transaction, where what
        // the refs lead to is read.
        let published = self.published_walked(&key.scope.repository_instance_id)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &key.scope.repository_instance_id)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        key.validate()?;
        if review {
            if key.scope != current.scope {
                return Err(Error::Unauthorized);
            }
            crate::tasks::reviewer(&mut tx, &self.spec, credential, &current)?;
        } else {
            check_key(key, &current.scope, &auth.actor_id)?;
        }
        let row =
            request_row(&mut tx, key)?.ok_or(Error::Precondition("request not registered"))?;
        let time = now(&mut tx)?;
        active(&row, time)?;
        if review && row.get::<_, String>("state") != "ready" {
            return Err(Error::Precondition("publication not ready for review"));
        }
        if unsigned(row.get("worker_lease_until"))? > time {
            return Err(Error::Precondition("publication worker already leased"));
        }
        let publication: Publication = row.get::<_, Jsonb>("publication").read()?;
        if !review {
            // writes are off while the lease is over, which is not what the
            // request is refused for: it comes again as it is
            crate::cloud::lease_over(&mut tx, &self.spec)?;
            alt_repository_service::authorize_intent(
                &publication,
                &current,
                &auth,
                self.limits,
                time,
            )?;
        }
        let manifest: Manifest = row.get::<_, Jsonb>("manifest").read()?;
        manifest.validate(self.limits)?;
        selected(&mut tx, key, &manifest, &published)?;
        let repository = &key.scope.repository_instance_id;
        let expires = unsigned(row.get("expires_at"))?;
        let lease_until = time
            .checked_add(lease_ms)
            .ok_or(Error::Limit("worker lifetime"))?
            .min(expires);
        let pin_id = upload_pin(key);
        let objects = catalog(&mut tx, key, time, max_objects)?;
        let digests: Vec<_> = objects
            .values()
            .map(|(_, digest, _)| digest.as_str())
            .collect();
        pin_many(&mut tx, repository, &pin_id, &digests, Some(expires))?;
        let fence=unsigned(tx.query_one("UPDATE alt_requests SET fence=fence+1,worker_lease_until=$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 RETURNING fence",&[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&integer(lease_until)?])?.get(0))?;
        let held = Held::of(&mut tx, key, &manifest, objects)?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        self.work(key, Some(fence), publication, manifest, held)
    }
    /// The selected publication that waits for review, for a current reviewer to
    /// read: as it is committed when the read begins, and without the lease. The
    /// request is neither held from its publisher nor changed, and nothing is
    /// decided through what is given here.
    pub fn read_review(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        max_objects: usize,
    ) -> Result<PublicationWork> {
        crate::reading::twice(|| self.read_review_once(credential, key, max_objects))
    }
    fn read_review_once(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        max_objects: usize,
    ) -> Result<PublicationWork> {
        if max_objects == 0 {
            return Err(Error::Limit("worker lease or object bound"));
        }
        let max_objects = max_objects.min(self.limits.graph_objects);
        let published = self.published_walked(&key.scope.repository_instance_id)?;
        let mut tx = crate::reading::transaction(&mut self.client, &self.spec)?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (row, _) = crate::review::request(&mut tx, &self.spec, credential, key)?;
        let publication: Publication = row.get::<_, Jsonb>("publication").read()?;
        let manifest: Manifest = row.get::<_, Jsonb>("manifest").read()?;
        manifest.validate(self.limits)?;
        selected(&mut tx, key, &manifest, &published)?;
        let time = now(&mut tx)?;
        let objects = catalog(&mut tx, key, time, max_objects)?;
        let held = Held::of(&mut tx, key, &manifest, objects)?;
        tx.commit()?;
        self.work(key, None, publication, manifest, held)
    }
    fn work(
        &mut self,
        key: &RequestKey,
        fence: Option<u64>,
        publication: Publication,
        manifest: Manifest,
        held: Held,
    ) -> Result<PublicationWork> {
        let published = self.published_walked_as(&key.scope.repository_instance_id, held.listed)?;
        Ok(PublicationWork {
            key: key.clone(),
            fence,
            publication,
            manifest,
            source: PinnedSource {
                bytes: crate::bytes::ReadSession::new(&self.bytes, &key.scope),
                objects: held.objects,
                artifacts: held.artifacts,
                locations: held.locations,
                segments: published.segments,
                reached: published.reached,
            },
        })
    }
}

type Objects = BTreeMap<ObjectId, (ObjectKind, String, u64)>;

/// what the transaction that gives out a request found for its source
struct Held {
    objects: Objects,
    artifacts: BTreeMap<String, u64>,
    locations: crate::segments::Locations,
    listed: reached::Listed,
}
impl Held {
    fn of(
        tx: &mut Transaction<'_>,
        key: &RequestKey,
        manifest: &Manifest,
        objects: Objects,
    ) -> Result<Self> {
        let repository = &key.scope.repository_instance_id;
        let artifacts: BTreeMap<String, u64> = manifest
            .entries
            .iter()
            .map(|e| (e.blake3.clone(), e.bytes))
            .collect();
        let all_digests: Vec<_> = objects
            .values()
            .map(|(_, digest, _)| digest.as_str())
            .chain(artifacts.keys().map(String::as_str))
            .collect();
        let locations = crate::segments::locations(tx, repository, &all_digests)?;
        let listed = reached::Listed::of(tx, repository)?;
        Ok(Self {
            objects,
            artifacts,
            locations,
            listed,
        })
    }
}

/// Every entry of the manifest is content the request has verified, stored or
/// given by a segment.
fn selected(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
    manifest: &Manifest,
    published: &reached::Published,
) -> Result<()> {
    let repository = &key.scope.repository_instance_id;
    for batch in manifest.entries.chunks(crate::database::BATCH_ROWS) {
        let digests: Vec<_> = batch.iter().map(|entry| entry.blake3.as_str()).collect();
        let content: BTreeMap<String, alt_pg::Row> = tx.query("SELECT c.blake3,c.verified,b.bytes,b.state FROM alt_request_content c LEFT JOIN alt_blobs b ON b.repository=c.repository AND b.blake3=c.blake3 WHERE c.repository=$1 AND c.epoch=$2 AND c.actor_id=$3 AND c.request_id=$4 AND c.blake3=ANY($5)", &[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&digests])?
            .into_iter().map(|row| (row.get(0), row)).collect();
        let oids: Vec<_> = batch
            .iter()
            .filter_map(|entry| entry.git_oid.as_deref())
            .collect();
        let mappings: BTreeMap<String, (String, String)> = tx
            .query(
                "SELECT oid,kind,blake3 FROM alt_objects WHERE repository=$1 AND oid=ANY($2)",
                &[repository, &oids],
            )?
            .into_iter()
            .map(|row| (row.get(0), (row.get(1), row.get(2))))
            .collect();
        for entry in batch {
            let row = content
                .get(&entry.blake3)
                .filter(|row| row.get::<_, bool>(1))
                .ok_or(Error::Precondition(
                    "selected content not uploaded or published",
                ))?;
            let stored = row.get::<_, Option<i64>>(2).map(unsigned).transpose()?
                == Some(entry.bytes)
                && row.get::<_, Option<String>>(3).as_deref() == Some("stored");
            if let Some(oid) = &entry.git_oid {
                let kind = crate::requests::git_kind(entry.kind)
                    .map_err(|_| Error::Integrity("Git mapping kind"))?;
                match mappings.get(oid) {
                    Some((actual_kind, actual_digest))
                        if actual_kind != kind || actual_digest != &entry.blake3 =>
                    {
                        return Err(Error::Integrity("Git mapping selection"));
                    }
                    Some(_) if stored => {}
                    // not in the content store: a segment has to give it
                    _ if published.holds_entry(entry)? => {}
                    _ => return Err(Error::Precondition("Git mapping not uploaded")),
                }
            } else if !stored {
                return Err(Error::Precondition("upload incomplete"));
            }
        }
    }
    Ok(())
}

/// The objects of the catalog the request may be read against: the published ones
/// and those the request itself verified.
fn catalog(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
    time: u64,
    max_objects: usize,
) -> Result<Objects> {
    let repository = &key.scope.repository_instance_id;
    let rows=tx.query("SELECT o.oid,o.kind,o.blake3,b.bytes FROM alt_objects o JOIN alt_blobs b ON b.repository=o.repository AND b.blake3=o.blake3 WHERE o.repository=$1 AND b.state='stored' AND EXISTS (SELECT 1 FROM alt_pins p WHERE p.repository=o.repository AND p.blake3=o.blake3 AND (p.expires_at IS NULL OR (p.pin_id=$2 AND p.expires_at>$3 AND EXISTS(SELECT 1 FROM alt_request_content c WHERE c.repository=o.repository AND c.epoch=$5 AND c.actor_id=$6 AND c.request_id=$7 AND c.blake3=o.blake3 AND c.verified)))) ORDER BY o.oid LIMIT $4",&[repository,&upload_pin(key),&integer(time)?,&integer((max_objects as u64).saturating_add(1))?,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?;
    if rows.len() > max_objects {
        return Err(Error::Limit("repository graph objects"));
    }
    let mut objects = BTreeMap::new();
    for row in rows {
        let oid: String = row.get(0);
        let kind: String = row.get(1);
        let digest: String = row.get(2);
        let bytes = unsigned(row.get(3))?;
        let oid = oid
            .parse()
            .map_err(|_| Error::Integrity("catalog object id"))?;
        let kind = kind
            .parse()
            .map_err(|_| Error::Integrity("catalog object kind"))?;
        objects.insert(oid, (kind, digest, bytes));
    }
    Ok(objects)
}
