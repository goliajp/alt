//! Authorized workbench projections over the publication authority.
use crate::Jsonb;
use crate::store::{authorize, credential_row, instance, integer, now, snapshot, unsigned};
use crate::{Credential, Error, Result, Store};
use alt_remote_protocol::{Publication, RequestKey, Scope};
use serde::Serialize;

#[derive(Serialize, alt_json::ToJson)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}
#[derive(Serialize, alt_json::ToJson)]
pub struct WorkbenchRepository {
    pub scope: Scope,
    pub default_branch: String,
    pub writes_enabled: bool,
    pub can_review: bool,
    pub can_publish: bool,
}
#[derive(Serialize, alt_json::ToJson)]
pub struct WorkbenchRequest {
    pub key: RequestKey,
    pub state: String,
    pub digest: String,
    pub roots: Vec<alt_remote_protocol::RecordRoot>,
}
pub struct PublishedRecord {
    pub record: alt_task_service::TaskRecord,
    pub payload: alt_task::Artifact,
    pub publication: crate::PublishedBundle,
}
fn page_size(limit: usize) -> Result<i64> {
    if limit == 0 || limit > 100 {
        return Err(Error::Limit("workbench page size"));
    }
    integer(limit as u64 + 1)
}
impl Store {
    pub fn workbench_repositories(
        &mut self,
        credential: &Credential,
        after: &str,
        limit: usize,
    ) -> Result<Page<WorkbenchRepository>> {
        let bound = page_size(limit)?;
        if after.len() > 128 {
            return Err(Error::Limit("repository cursor"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let actor: String = credential_row(&mut tx, &self.spec, credential)?.get("actor_id");
        if crate::delegation::parent(&mut tx, &credential.digest)?.is_some() {
            return Err(Error::Unauthorized);
        }
        let rows=tx.query("SELECT r.id,r.recovery_epoch,r.default_branch,r.writes_enabled,g.publish_records,COALESCE(v.can_review,false) FROM alt_repositories r JOIN alt_grants g ON g.repository=r.id AND g.actor_id=$1 LEFT JOIN alt_review_grants v ON v.repository=r.id AND v.actor_id=$1 WHERE g.can_read AND r.id>$2 ORDER BY r.id LIMIT $3", &[&actor,&after,&bound])?;
        let service_writes = crate::cloud::writes_enabled(&mut tx, &self.spec)?;
        let has_more = rows.len() > limit;
        let items = rows
            .into_iter()
            .take(limit)
            .map(|row| {
                Ok(WorkbenchRepository {
                    scope: Scope {
                        installation_id: self.spec.installation_id.clone(),
                        repository_instance_id: row.get(0),
                        recovery_epoch: unsigned(row.get(1))?,
                    },
                    default_branch: row.get(2),
                    writes_enabled: row.get::<_, bool>(3) && service_writes,
                    can_publish: row.get(4),
                    can_review: row.get(5),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let next = if has_more {
            items.last().map(|v| v.scope.repository_instance_id.clone())
        } else {
            None
        };
        tx.commit()?;
        Ok(Page { items, next })
    }
    /// Only current ready selections visible to a current repository reviewer.
    /// Uploading/private requests are never included, even for administrators.
    pub fn workbench_inbox(
        &mut self,
        credential: &Credential,
        after: &str,
        limit: usize,
    ) -> Result<Page<WorkbenchRequest>> {
        let bound = page_size(limit)?;
        if after.len() > 400 {
            return Err(Error::Limit("inbox cursor"));
        }
        let parts = if after.is_empty() {
            vec!["", "", ""]
        } else {
            after.split('/').collect::<Vec<_>>()
        };
        if parts.len() != 3 {
            return Err(Error::Precondition("inbox cursor"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let actor: String = credential_row(&mut tx, &self.spec, credential)?.get("actor_id");
        if crate::delegation::parent(&mut tx, &credential.digest)?.is_some() {
            return Err(Error::Unauthorized);
        }
        let time = integer(now(&mut tx)?)?;
        let rows=tx.query("SELECT q.repository,q.epoch,q.actor_id,q.request_id,q.digest,q.publication->'record_roots' AS roots FROM alt_requests q JOIN alt_repositories r ON r.id=q.repository AND r.recovery_epoch=q.epoch JOIN alt_grants g ON g.repository=q.repository AND g.actor_id=$1 JOIN alt_review_grants v ON v.repository=q.repository AND v.actor_id=$1 WHERE g.can_read AND v.can_review AND r.writes_enabled AND q.state='ready' AND q.expires_at>$2 AND (q.repository,q.actor_id,q.request_id)>($3,$4,$5) AND EXISTS(SELECT 1 FROM jsonb_array_elements(q.publication->'record_roots') root WHERE root->>'kind' IN ('integration','delivery')) ORDER BY q.repository,q.actor_id,q.request_id LIMIT $6", &[&actor,&time,&parts[0],&parts[1],&parts[2],&bound])?;
        let has_more = rows.len() > limit;
        let items = rows
            .into_iter()
            .take(limit)
            .map(|row| {
                Ok(WorkbenchRequest {
                    key: RequestKey {
                        scope: Scope {
                            installation_id: self.spec.installation_id.clone(),
                            repository_instance_id: row.get(0),
                            recovery_epoch: unsigned(row.get(1))?,
                        },
                        actor_id: row.get(2),
                        request_id: row.get(3),
                    },
                    state: "ready".into(),
                    digest: row.get(4),
                    roots: row.get::<_, Jsonb>(5).read()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let next = if has_more {
            items.last().map(|item| {
                format!(
                    "{}/{}/{}",
                    item.key.scope.repository_instance_id, item.key.actor_id, item.key.request_id
                )
            })
        } else {
            None
        };
        tx.commit()?;
        Ok(Page { items, next })
    }
    pub fn published_record(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        root: &alt_remote_protocol::RecordRoot,
        max_bytes: usize,
    ) -> Result<PublishedRecord> {
        use std::io::Read;
        root.validate()?;
        if max_bytes == 0 || max_bytes > 16 * 1024 * 1024 {
            return Err(Error::Limit("record projection bytes"));
        }
        let kind = alt_json::to_value(root.kind)?
            .as_str()
            .ok_or(Error::Integrity("record kind"))?
            .to_owned();
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        authorize(&mut tx, &self.spec, credential, &current)?;
        if current.scope != *scope {
            return Err(Error::Unauthorized);
        }
        let row=tx.query_opt("SELECT h.payload_blake3,b.bytes,h.publication,h.manifest,h.receipt FROM (SELECT br.repository,br.payload_blake3,q.digest,q.publication,q.manifest,q.result->'receipt' AS receipt FROM alt_task_bundle_records br JOIN alt_requests q ON q.repository=br.repository AND q.epoch=br.epoch AND q.actor_id=br.actor_id AND q.request_id=br.request_id WHERE br.repository=$1 AND br.kind=$2 AND br.record_id=$3 AND q.state='committed' UNION ALL SELECT r.repository,r.payload_blake3,p.digest,p.publication,p.manifest,p.receipt FROM alt_archived_records r JOIN alt_archived_publications p ON p.repository=r.repository AND p.digest=r.digest WHERE r.repository=$1 AND r.kind=$2 AND r.record_id=$3) h JOIN alt_blobs b ON b.repository=h.repository AND b.blake3=h.payload_blake3 WHERE b.state='stored' ORDER BY h.digest,h.payload_blake3 LIMIT 1", &[&scope.repository_instance_id,&kind,&root.id])?.ok_or(Error::Unauthorized)?;
        let payload = alt_task::Artifact {
            blake3: row.get(0),
            bytes: unsigned(row.get(1))?,
        };
        if payload.bytes > max_bytes as u64 {
            return Err(Error::Limit("record projection bytes"));
        }
        let location =
            crate::segments::location(&mut tx, &scope.repository_instance_id, &payload.blake3)?;
        let file =
            self.bytes
                .read_payload(scope, &payload.blake3, payload.bytes, location.as_ref())?;
        let publication: Publication = row.get::<_, Jsonb>(2).read()?;
        let manifest = row.get::<_, Jsonb>(3).read()?;
        let receipt: alt_remote_protocol::Receipt = row.get::<_, Jsonb>(4).read()?;
        let provenance = crate::provenance::load(
            &mut tx,
            scope,
            &publication,
            &receipt,
            &self.signing_key,
            self.limits,
        )?;
        tx.commit()?;
        let mut raw = Vec::new();
        file.take(max_bytes as u64 + 1).read_to_end(&mut raw)?;
        if raw.len() as u64 != payload.bytes
            || alt_hash::blake3::hash(&raw).to_hex().as_str() != payload.blake3
        {
            return Err(Error::Integrity("record projection payload"));
        }
        let key = alt_task_service::RecordKey {
            kind: alt_json::from_value(alt_json::Value::String(kind))?,
            id: root.id.clone(),
        };
        let record = alt_task_service::TaskRecord::decode(&key, &raw, max_bytes)?;
        Ok(PublishedRecord {
            record,
            payload,
            publication: crate::PublishedBundle {
                publication,
                manifest,
                receipt,
                provenance,
            },
        })
    }
}
