//! A repair request can name one published native Task. A claim is not a
//! candidate, execution result, review or Delivery; those retain their own graph.
use crate::change_channel::{authority, endpoint, installed};
use crate::database::Transaction;
use crate::store::{authorize, instance, integer, snapshot};
use crate::{Credential, Error, InstanceSpec, PublicationWork, Result, Store};
use alt_remote_protocol::Scope;
use alt_repository_service::{Authorization, RepositorySnapshot};
use alt_task_service::{TaskRecord, VerifiedTaskGraph};
use golia_change_link as link;

pub use alt_remote_protocol::change_links::RepairTaskClaim;

fn source(tx: &mut Transaction<'_>, scope: &Scope, digest: &str) -> Result<link::RepairSource> {
    crate::bytes::check_digest(digest)?;
    let history = crate::link_history::get(tx, &scope.repository_instance_id, digest)?;
    let acknowledgement = history
        .acknowledgement
        .ok_or(Error::Precondition("repair request not accepted"))?;
    let source = link::RepairSource {
        request: history.statement,
        source_public_key: history.source_public_key,
        acceptance: acknowledgement.receipt,
        receiver_public_key: acknowledgement.receiver_public_key,
    };
    source
        .validate()
        .map_err(|_| Error::Precondition("repair source not accepted"))?;
    Ok(source)
}

fn current_source(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    current: &RepositorySnapshot,
    auth: &Authorization,
    digest: &str,
) -> Result<link::RepairSource> {
    let scope = &current.scope;
    let source = source(tx, scope, digest)?;
    let event = &source.request.event;
    let channel = installed(tx, scope, &event.channel)?;
    if event.receiver != endpoint(spec, scope)
        || event.issuer != channel.peer
        || source.source_public_key != channel.peer_public_key
    {
        return Err(Error::Unauthorized);
    }
    authority(
        tx,
        spec,
        &Credential {
            digest: channel.local_credential_digest,
        },
        current,
        true,
    )?;
    let link::Body::RepairRequest(request) = &event.body else {
        return Err(Error::Integrity("repair source kind"));
    };
    if !current.writes_enabled
        || !auth.publish_records
        || !current.refs.contains_key(&request.target_ref)
        || !auth
            .grants
            .iter()
            .any(|g| g.update && g.refs.matches(&request.target_ref))
    {
        return Err(Error::Unauthorized);
    }
    if tx.query_opt("SELECT 1 FROM alt_link_current WHERE repository=$1 AND epoch=$2 AND channel=$3 AND entity=$4 AND kind='repair_request' AND event_digest=$5", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&event.channel,&event.entity,&digest])?.is_none() {
        return Err(Error::Precondition("repair request superseded"));
    }
    Ok(source)
}

pub(crate) fn claim_published_tasks(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    work: &PublicationWork,
    tasks: Option<&VerifiedTaskGraph>,
    current: &RepositorySnapshot,
    auth: &Authorization,
    publication_digest: &str,
) -> Result<()> {
    let Some(tasks) = tasks else { return Ok(()) };
    let scope = &work.key.scope;
    for selection in tasks.records() {
        let TaskRecord::Task(task) = tasks.prepared().record(&selection.record)? else {
            continue;
        };
        let Some(proof) = &task.spec.repair_source else {
            continue;
        };
        // A previously published/imported record is historical. Republishing
        // it must neither recreate a connector nor acquire a new repair claim.
        if tx.query_opt("SELECT 1 FROM alt_task_records WHERE repository=$1 AND kind='task' AND record_id=$2", &[&scope.repository_instance_id,&selection.record.id])?.is_some() { continue }
        let digest = proof
            .request
            .digest()
            .map_err(|_| Error::Integrity("repair digest"))?;
        let accepted = current_source(tx, spec, current, auth, &digest)?;
        if **proof != accepted {
            return Err(Error::Precondition(
                "repair source differs from accepted original",
            ));
        }
        let event = &accepted.request.event;
        if let Some(row) = tx.query_opt("SELECT repair_digest,task_id FROM alt_link_task_claims WHERE repository=$1 AND epoch=$2 AND channel=$3 AND entity=$4", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&event.channel,&event.entity])? {
            if row.get::<_,String>(0) != digest || row.get::<_,String>(1) != selection.record.id {
                return Err(Error::Precondition("repair request already bound to another task"));
            }
            continue;
        }
        tx.execute("INSERT INTO alt_link_task_claims(repository,epoch,channel,entity,repair_digest,task_id,publication_digest,actor_id,request_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&event.channel,&event.entity,&digest,&selection.record.id,&publication_digest,&auth.actor_id,&work.key.request_id])?;
    }
    Ok(())
}

impl Store {
    /// Obtain a historical proof for explicit local task creation after current
    /// repository, integration and branch authority checks. It grants no right
    /// to execute code and does not reserve or manufacture a native Task.
    pub fn repair_task_source(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
    ) -> Result<link::RepairSource> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        if current.scope != *scope {
            return Err(Error::Unauthorized);
        }
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        let value = current_source(&mut tx, &self.spec, &current, &auth, digest)?;
        tx.commit()?;
        Ok(value)
    }
    /// Reads an already committed claim even when its integration was disabled.
    /// Content access remains current; no signature substitutes for that check.
    pub fn repair_task_claim(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
    ) -> Result<Option<RepairTaskClaim>> {
        crate::bytes::check_digest(digest)?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        if current.scope != *scope {
            return Err(Error::Unauthorized);
        }
        authorize(&mut tx, &self.spec, credential, &current)?;
        let row = tx.query_opt("SELECT task_id,publication_digest FROM alt_link_task_claims WHERE repository=$1 AND epoch=$2 AND repair_digest=$3", &[&scope.repository_instance_id,&integer(scope.recovery_epoch)?,&digest])?;
        Ok(row.map(|row| RepairTaskClaim {
            repair_request_digest: digest.into(),
            task: row.get(0),
            publication_digest: row.get(1),
        }))
    }
}
