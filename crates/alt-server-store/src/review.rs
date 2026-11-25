//! What a reviewer reads of a request that waits for review.
//!
//! None of it decides anything, so none of it takes the lease of the request or
//! the row of its repository: each is one view of what is committed as it begins,
//! and the publisher of the request and other reviewers go on beside it. A request
//! that is published or ended meanwhile is no longer ready to the next read.

use crate::committed::request_seen;
use crate::requests::active;
use crate::store::{instance, integer, now};
use crate::tasks::{approve_current, check_pins, check_selection, collector_policy};
use crate::{Credential, Error, InstanceSpec, Jsonb, PublicationWork, Result, Store, reading};
use alt_remote_protocol::{Manifest, RequestKey, ReviewSelection};
use alt_repository_service::{RepositorySnapshot, VerifiedGraph};
use alt_task_service::VerifiedTaskGraph;
use std::collections::BTreeMap;

/// The request a current reviewer may read, and its repository, as the read sees them.
pub(crate) fn request(
    tx: &mut crate::database::Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
    key: &RequestKey,
) -> Result<(alt_pg::Row, RepositorySnapshot)> {
    key.validate()?;
    let current = reading::snapshot(tx, spec, &key.scope.repository_instance_id)?;
    crate::tasks::reviewer(tx, spec, credential, &current)?;
    if current.scope != key.scope {
        return Err(Error::Unauthorized);
    }
    let row = request_seen(tx, key)?.ok_or(Error::Unauthorized)?;
    active(&row, now(tx)?)?;
    if row.get::<_, String>("state") != "ready" {
        return Err(Error::Precondition("publication not ready for review"));
    }
    Ok((row, current))
}

impl Store {
    /// A current reviewer inspects the exact selected pending publication.
    /// The response is received history plus present refs, not approval.
    pub fn review_selection(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<ReviewSelection> {
        reading::twice(|| self.review_selection_once(credential, key))
    }
    fn review_selection_once(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<ReviewSelection> {
        let mut tx = reading::transaction(&mut self.client, &self.spec)?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (row, current) = request(&mut tx, &self.spec, credential, key)?;
        let (collector_generation, _) =
            crate::tasks::collector_policy(&mut tx, &key.scope.repository_instance_id)?;
        let result = ReviewSelection {
            publication: row.get::<_, Jsonb>("publication").read()?,
            manifest: row.get::<_, Jsonb>("manifest").read()?,
            current_refs: current
                .refs
                .into_iter()
                .map(|(name, oid)| (name, oid.to_string()))
                .collect(),
            policy_version: current.policy_version.to_string(),
            runtime_generation: self.spec.runtime_generation.to_string(),
            collector_generation: collector_generation.to_string(),
            revocation_generation: current.revocation_generation.to_string(),
        };
        tx.commit()?;
        Ok(result)
    }
    pub fn open_review_content(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<(u64, crate::StoredReader)> {
        reading::twice(|| self.open_review_content_once(credential, key, digest))
    }
    fn open_review_content_once(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<(u64, crate::StoredReader)> {
        alt_task::check_id(digest).map_err(|_| Error::Unauthorized)?;
        let mut tx = reading::transaction(&mut self.client, &self.spec)?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (row, _) = request(&mut tx, &self.spec, credential, key)?;
        let manifest: Manifest = row.get::<_, Jsonb>("manifest").read()?;
        let entry = manifest
            .entries
            .iter()
            .find(|entry| entry.blake3 == digest)
            .ok_or(Error::Unauthorized)?;
        let time = integer(now(&mut tx)?)?;
        if tx.query_opt("SELECT 1 FROM alt_pins p JOIN alt_blobs b ON b.repository=p.repository AND b.blake3=p.blake3 WHERE p.repository=$1 AND p.pin_id=$2 AND p.blake3=$3 AND p.expires_at>$4 AND b.state='stored' AND b.bytes=$5", &[&key.scope.repository_instance_id,&crate::requests::upload_pin(key),&digest,&time,&integer(entry.bytes)?])?.is_none() {
            // a selected Git object that was published earlier was not uploaded again
            let entry = entry.clone();
            tx.commit()?;
            let published = self.published(&key.scope.repository_instance_id)?;
            if published.holds_entry(&entry)?
                && let Some(content) =
                    crate::transport::segment_content(&published.segments, digest, self.restoring.as_ref())?
            {
                return Ok(content);
            }
            return Err(Error::Precondition("review content pin expired"));
        }
        let location =
            crate::segments::location(&mut tx, &key.scope.repository_instance_id, digest)?;
        let file = self
            .bytes
            .read_payload(&key.scope, digest, entry.bytes, location.as_ref())?;
        tx.commit()?;
        Ok((entry.bytes, file))
    }
}

#[derive(alt_json::ToJson)]
pub struct CurrentReviewAssessment {
    pub approved: bool,
    pub selection: alt_remote_protocol::ReviewSelection,
    pub evidence: alt_task_service::EvidenceAssessment,
    pub eligible: bool,
    pub reason: &'static str,
}
impl Store {
    /// What the server would make of the integration now. Nothing is recorded.
    pub fn inspect_current_integration(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: &VerifiedTaskGraph,
        integration_id: &str,
    ) -> Result<CurrentReviewAssessment> {
        reading::twice(|| {
            self.inspect_current_integration_once(credential, work, graph, tasks, integration_id)
        })
    }
    fn inspect_current_integration_once(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: &VerifiedTaskGraph,
        integration_id: &str,
    ) -> Result<CurrentReviewAssessment> {
        check_selection(work, Some(tasks))?;
        let review = crate::tasks::integration_review(work, tasks, integration_id)?;
        work.check_read_integrity()?;
        let mut tx = reading::transaction(&mut self.client, &self.spec)?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let (_, mut current) = request(&mut tx, &self.spec, credential, &work.key)?;
        crate::publication::check_graph_pins(&mut tx, work, graph)?;
        check_pins(&mut tx, work, graph, Some(tasks))?;
        let (generation, policy) =
            collector_policy(&mut tx, &current.scope.repository_instance_id)?;
        let target = review
            .target_expected
            .keys()
            .filter_map(|name| {
                current
                    .refs
                    .get(name)
                    .map(|id| (name.clone(), id.to_string()))
            })
            .collect::<BTreeMap<_, _>>();
        let evidence = tasks.assess_evidence(&review.evidence, &target, &policy, now(&mut tx)?)?;
        let eligible =
            current.writes_enabled && target == review.target_expected && evidence.eligible;
        let reason = if !current.writes_enabled {
            "repository_read_only"
        } else if target != review.target_expected {
            "target_changed"
        } else if !evidence.eligible {
            "evidence_not_accepted"
        } else {
            "current_policy_accepted"
        };
        let approved = if eligible {
            match approve_current(
                &mut tx,
                &self.spec,
                work,
                Some(tasks),
                &mut current,
                self.limits,
            ) {
                Ok(()) => true,
                Err(Error::Unauthorized | Error::Precondition(_)) => false,
                Err(error) => return Err(error),
            }
        } else {
            false
        };
        let selection = alt_remote_protocol::ReviewSelection {
            publication: work.publication.clone(),
            manifest: work.manifest.clone(),
            current_refs: current
                .refs
                .into_iter()
                .map(|(name, oid)| (name, oid.to_string()))
                .collect(),
            policy_version: current.policy_version.to_string(),
            runtime_generation: self.spec.runtime_generation.to_string(),
            collector_generation: generation.to_string(),
            revocation_generation: current.revocation_generation.to_string(),
        };
        tx.commit()?;
        Ok(CurrentReviewAssessment {
            approved,
            selection,
            evidence,
            eligible,
            reason,
        })
    }
}
