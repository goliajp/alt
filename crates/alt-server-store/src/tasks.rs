use crate::database::Transaction;
use crate::requests::{pin, upload_pin};
use crate::store::{authorize, instance, integer, now, require_admin, snapshot, unsigned};
use crate::{Credential, Error, InstanceSpec, PublicationWork, Result, Store};
use alt_repository_service::{RepositorySnapshot, VerifiedGraph};
use alt_task_service::{CollectorPolicy, RecordKey, RecordKind, TaskRecord, VerifiedTaskGraph};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn check_selection(
    work: &PublicationWork,
    tasks: Option<&VerifiedTaskGraph>,
) -> Result<()> {
    let Some(tasks) = tasks else {
        if work.publication.record_roots.is_empty() {
            return Ok(());
        }
        return Err(Error::Precondition("task graph validation required"));
    };
    if work
        .publication
        .record_roots
        .iter()
        .collect::<BTreeSet<_>>()
        != tasks.roots().iter().collect()
        || work
            .manifest
            .entries
            .iter()
            .filter(|e| e.kind == alt_remote_protocol::ContentKind::TaskBundle)
            .count()
            != 1
        || !work.manifest.entries.iter().any(|e| {
            e.kind == alt_remote_protocol::ContentKind::TaskBundle
                && e.blake3 == tasks.bundle_artifact().blake3
                && e.bytes == tasks.bundle_artifact().bytes
        })
    {
        return Err(Error::Integrity("verified task publication binding"));
    }
    Ok(())
}
pub(crate) fn check_pins(
    tx: &mut Transaction<'_>,
    work: &PublicationWork,
    graph: &VerifiedGraph,
    tasks: Option<&VerifiedTaskGraph>,
) -> Result<()> {
    let Some(tasks) = tasks else {
        return Ok(());
    };
    let repository = &work.key.scope.repository_instance_id;
    let pin_id = upload_pin(&work.key);
    for artifact in tasks
        .facts()
        .retained_artifacts()
        .iter()
        .chain(std::iter::once(tasks.bundle_artifact()))
    {
        if !work
            .manifest
            .entries
            .iter()
            .any(|e| e.blake3 == artifact.blake3 && e.bytes == artifact.bytes)
        {
            return Err(Error::Integrity("task artifact not selected"));
        }
    }
    // Receivers download the immutable signed manifest, including explicitly
    // selected objects outside the task's minimum graph. All of it must remain
    // pinned, not just the subset needed to validate today's task roots.
    let time = integer(now(tx)?)?;
    for entry in &work.manifest.entries {
        // a Git object a segment holds needs no copy in the content store
        if work.source.settled_entry(entry)? {
            continue;
        }
        let row = tx.query_opt("SELECT b.bytes FROM alt_blobs b JOIN alt_pins p ON p.repository=b.repository AND p.blake3=b.blake3 WHERE b.repository=$1 AND b.blake3=$2 AND b.state='stored' AND p.pin_id=$3 AND p.expires_at>$4", &[repository,&entry.blake3,&pin_id,&time])?.ok_or(Error::Precondition("task selection pin expired"))?;
        if unsigned(row.get(0))? != entry.bytes {
            return Err(Error::Integrity("task selection size"));
        }
    }
    for oid in tasks.git_objects() {
        graph.node(*oid)?;
        if !work.source.objects.contains_key(oid) && !in_segment(work, oid) {
            return Err(Error::Integrity(
                "task Git object outside authorized source",
            ));
        }
    }
    Ok(())
}
/// published earlier, by either protocol
fn in_segment(work: &PublicationWork, oid: &alt_git_codec::ObjectId) -> bool {
    <[u8; 20]>::try_from(oid.as_bytes()).is_ok_and(|key| work.source.segments.locate_any(&key))
}
pub(crate) fn retain(
    tx: &mut Transaction<'_>,
    work: &PublicationWork,
    tasks: Option<&VerifiedTaskGraph>,
    sequence: u64,
    digest: &str,
) -> Result<()> {
    let Some(tasks) = tasks else {
        return Ok(());
    };
    let repository = &work.key.scope.repository_instance_id;
    let bundle = &tasks.bundle_artifact().blake3;
    let pin_id = format!("task-bundle:{bundle}");
    // Records, attachments and the bundle stay in the content store for good. The
    // Git objects the manifest selects are held by the pack segments instead.
    for entry in &work.manifest.entries {
        if entry.git_oid.is_none() {
            pin(tx, repository, &pin_id, &entry.blake3, None)?;
        }
    }
    for selection in tasks.records() {
        tx.execute("INSERT INTO alt_task_records(repository,kind,record_id,payload_blake3) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING", &[repository,&selection.record.kind.as_str(),&selection.record.id,&selection.payload.blake3])?;
    }
    for root in tasks.roots() {
        let kind = serde_json::to_value(root.kind)?
            .as_str()
            .ok_or(Error::Integrity("task root kind"))?
            .to_owned();
        tx.execute("INSERT INTO alt_task_roots(repository,kind,record_id,publication_digest) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING", &[repository,&kind,&root.id,&digest])?;
    }
    tx.execute("INSERT INTO alt_task_bundles(repository,bundle_blake3,epoch,actor_id,request_id) VALUES($1,$2,$3,$4,$5)", &[repository,bundle,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id])?;
    for selection in tasks.records() {
        tx.execute("INSERT INTO alt_task_bundle_records(repository,epoch,actor_id,request_id,kind,record_id,payload_blake3) VALUES($1,$2,$3,$4,$5,$6,$7)", &[repository,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id,&selection.record.kind.as_str(),&selection.record.id,&selection.payload.blake3])?;
    }
    for root in tasks.roots() {
        let kind = serde_json::to_value(root.kind)?
            .as_str()
            .ok_or(Error::Integrity("task root kind"))?
            .to_owned();
        tx.execute("INSERT INTO alt_task_bundle_roots(repository,epoch,actor_id,request_id,kind,record_id) VALUES($1,$2,$3,$4,$5,$6)", &[repository,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id,&kind,&root.id])?;
    }
    tx.execute("UPDATE alt_current_approvals SET consumed_sequence=$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[repository,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id,&integer(sequence)?])?;
    Ok(())
}

pub(crate) fn collector_policy(
    tx: &mut Transaction<'_>,
    repository: &str,
) -> Result<(u64, CollectorPolicy)> {
    match tx.query_opt(
        "SELECT generation,policy FROM alt_collector_policies WHERE repository=$1",
        &[&repository],
    )? {
        Some(row) => Ok((unsigned(row.get(0))?, serde_json::from_value(row.get(1))?)),
        None => Ok((0, CollectorPolicy::default())),
    }
}
pub(crate) fn reviewer(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    credential: &Credential,
    current: &RepositorySnapshot,
) -> Result<String> {
    if crate::delegation::parent(tx, &credential.digest)?.is_some() {
        return Err(Error::Unauthorized);
    }
    let auth = authorize(tx, spec, credential, current)?;
    if tx
        .query_opt(
            "SELECT 1 FROM alt_review_grants WHERE repository=$1 AND actor_id=$2 AND can_review",
            &[&current.scope.repository_instance_id, &auth.actor_id],
        )?
        .is_none()
    {
        return Err(Error::Unauthorized);
    }
    Ok(auth.actor_id)
}
fn integration_review<'a>(
    work: &PublicationWork,
    tasks: &'a VerifiedTaskGraph,
    integration_id: &str,
) -> Result<&'a alt_task::Review> {
    let TaskRecord::Integration(integration) = tasks.prepared().record(&RecordKey {
        kind: RecordKind::Integration,
        id: integration_id.into(),
    })?
    else {
        return Err(Error::Integrity("approval integration kind"));
    };
    let TaskRecord::Review(review) = tasks.prepared().record(&RecordKey {
        kind: RecordKind::Review,
        id: integration.review.clone(),
    })?
    else {
        return Err(Error::Integrity("approval review kind"));
    };
    if work.publication.refs.len() != 2
        || !work.publication.refs.iter().any(|r| {
            r.name == integration.target
                && r.expected.as_ref() == Some(&integration.before)
                && r.new.as_ref() == Some(&integration.commit)
                && !r.force
        })
        || !work.publication.refs.iter().any(|r| {
            r.name == "refs/notes/alt/meta"
                && r.expected == integration.notes_before
                && r.new.as_ref() == Some(&integration.notes_after)
                && !r.force
        })
    {
        return Err(Error::Precondition(
            "approval does not match exact integration refs",
        ));
    }
    Ok(review)
}
fn assessment(
    tasks: &VerifiedTaskGraph,
    review: &alt_task::Review,
    current: &RepositorySnapshot,
    policy: &CollectorPolicy,
    time: u64,
) -> Result<()> {
    let target: BTreeMap<_, _> = review
        .target_expected
        .keys()
        .filter_map(|name| {
            current
                .refs
                .get(name)
                .map(|oid| (name.clone(), oid.to_string()))
        })
        .collect();
    if target != review.target_expected
        || !tasks
            .assess_evidence(&review.evidence, &target, policy, time)?
            .eligible
    {
        return Err(Error::Precondition(
            "candidate evidence is not currently acceptable",
        ));
    }
    Ok(())
}
impl Store {
    pub fn set_review_grant(
        &mut self,
        admin: &Credential,
        repository: &str,
        actor: &str,
        can_review: bool,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        snapshot(&mut tx, &self.spec, repository)?;
        require_admin(&mut tx, &self.spec, admin)?;
        tx.execute("INSERT INTO alt_review_grants VALUES($1,$2,$3) ON CONFLICT(repository,actor_id) DO UPDATE SET can_review=excluded.can_review", &[&repository,&actor,&can_review])?;
        tx.execute(
            "UPDATE alt_repositories SET revocation_generation=revocation_generation+1 WHERE id=$1",
            &[&repository],
        )?;
        crate::browser::audit_admin(
            &mut tx,
            &self.spec,
            admin,
            "repository.review_grant",
            repository,
            serde_json::json!({"actor_id":actor,"can_review":can_review}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn set_collector_policy(
        &mut self,
        admin: &Credential,
        repository: &str,
        policy: &CollectorPolicy,
    ) -> Result<()> {
        policy.validate()?;
        if serde_json::to_vec(policy)?.len() > self.limits.request_bytes {
            return Err(Error::Limit("collector policy bytes"));
        }
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        snapshot(&mut tx, &self.spec, repository)?;
        require_admin(&mut tx, &self.spec, admin)?;
        tx.execute("INSERT INTO alt_collector_policies VALUES($1,1,$2) ON CONFLICT(repository) DO UPDATE SET generation=alt_collector_policies.generation+1,policy=excluded.policy", &[&repository,&serde_json::to_value(policy)?])?;
        crate::browser::audit_admin(
            &mut tx,
            &self.spec,
            admin,
            "repository.collector_policy",
            repository,
            serde_json::json!({"policy":policy}),
        )?;
        tx.commit()?;
        Ok(())
    }
    /// A separate current decision by an authorized reviewer. Historical Review
    /// records alone never populate the server's approval table.
    #[allow(clippy::too_many_arguments)]
    pub fn approve_integration(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: &VerifiedTaskGraph,
        integration_id: &str,
        inspected: &alt_remote_protocol::ApprovalContext,
        ttl_ms: u64,
    ) -> Result<()> {
        if ttl_ms == 0 || ttl_ms > 86_400_000 {
            return Err(Error::Limit("approval lifetime"));
        }
        check_selection(work, Some(tasks))?;
        let review = integration_review(work, tasks, integration_id)?;
        work.check_read_integrity()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        let actor = reviewer(&mut tx, &self.spec, credential, &current)?;
        if current.scope != work.key.scope || !current.writes_enabled {
            return Err(Error::Precondition(
                "approval scope or read-only repository",
            ));
        }
        let row = crate::publication::worker(&mut tx, work)?;
        if row.get::<_, String>("state") != "ready" {
            return Err(Error::Precondition("publication not ready for review"));
        }
        crate::publication::check_graph_pins(&mut tx, work, graph)?;
        check_pins(&mut tx, work, graph, Some(tasks))?;
        let (generation, policy) =
            collector_policy(&mut tx, &current.scope.repository_instance_id)?;
        let target_refs: BTreeMap<_, _> = work
            .publication
            .refs
            .iter()
            .map(|update| {
                (
                    update.name.clone(),
                    current.refs.get(&update.name).map(ToString::to_string),
                )
            })
            .collect();
        if inspected.request_digest != row.get::<_, String>("digest")
            || inspected.runtime_generation != self.spec.runtime_generation.to_string()
            || inspected.policy_version != current.policy_version.to_string()
            || inspected.collector_generation != generation.to_string()
            || inspected.revocation_generation != current.revocation_generation.to_string()
            || inspected.target_refs != target_refs
        {
            return Err(Error::Precondition(
                "inspected approval context changed; inspect again",
            ));
        }
        let time = now(&mut tx)?;
        assessment(tasks, review, &current, &policy, time)?;
        let expires = integer(
            time.checked_add(ttl_ms)
                .ok_or(Error::Limit("approval lifetime"))?,
        )?;
        tx.execute("INSERT INTO alt_current_approvals(repository,epoch,actor_id,request_id,request_digest,bundle_blake3,integration_id,review_id,reviewer,reviewer_credential,policy_version,collector_generation,revocation_generation,approved_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) ON CONFLICT(repository,epoch,actor_id,request_id) DO UPDATE SET request_digest=excluded.request_digest,bundle_blake3=excluded.bundle_blake3,integration_id=excluded.integration_id,review_id=excluded.review_id,reviewer=excluded.reviewer,reviewer_credential=excluded.reviewer_credential,policy_version=excluded.policy_version,collector_generation=excluded.collector_generation,revocation_generation=excluded.revocation_generation,approved_at=excluded.approved_at,expires_at=excluded.expires_at,consumed_sequence=NULL", &[&work.key.scope.repository_instance_id,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id,&row.get::<_,String>("digest"),&tasks.bundle_artifact().blake3,&integration_id,&integration_review_id(tasks,integration_id)?,&actor,&credential.digest,&integer(current.policy_version)?,&integer(generation)?,&integer(current.revocation_generation)?,&integer(time)?,&expires])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(())
    }
}
fn integration_review_id(tasks: &VerifiedTaskGraph, id: &str) -> Result<String> {
    match tasks.prepared().record(&RecordKey {
        kind: RecordKind::Integration,
        id: id.into(),
    })? {
        TaskRecord::Integration(v) => Ok(v.review.clone()),
        _ => Err(Error::Integrity("integration record kind")),
    }
}
pub(crate) fn approve_current(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    work: &PublicationWork,
    tasks: Option<&VerifiedTaskGraph>,
    current: &mut RepositorySnapshot,
    limits: alt_remote_protocol::Limits,
) -> Result<()> {
    let Some(tasks) = tasks else {
        return Ok(());
    };
    if work.publication.refs.is_empty() {
        return Ok(());
    }
    let row = tx.query_opt("SELECT * FROM alt_current_approvals WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[&work.key.scope.repository_instance_id,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id])?.ok_or(Error::Precondition("current integration approval required"))?;
    let (generation, policy) = collector_policy(tx, &current.scope.repository_instance_id)?;
    let time = now(tx)?;
    let digest = work.publication.digest(limits)?;
    if row.get::<_, String>("request_digest") != digest
        || row.get::<_, String>("bundle_blake3") != tasks.bundle_artifact().blake3
        || unsigned(row.get("policy_version"))? != current.policy_version
        || unsigned(row.get("collector_generation"))? != generation
        || unsigned(row.get("revocation_generation"))? != current.revocation_generation
        || unsigned(row.get("expires_at"))? <= time
        || row.get::<_, Option<i64>>("consumed_sequence").is_some()
    {
        return Err(Error::Precondition("integration approval stale"));
    }
    let credential = Credential {
        digest: row.get("reviewer_credential"),
    };
    if reviewer(tx, spec, &credential, current)? != row.get::<_, String>("reviewer") {
        return Err(Error::Unauthorized);
    }
    let integration_id: String = row.get("integration_id");
    let review = integration_review(work, tasks, &integration_id)?;
    if integration_review_id(tasks, &integration_id)? != row.get::<_, String>("review_id") {
        return Err(Error::Integrity("approval review binding"));
    }
    assessment(tasks, review, current, &policy, time)?;
    current.approved_publications.insert(digest);
    Ok(())
}

#[derive(serde::Serialize)]
pub struct CurrentReviewAssessment {
    pub approved: bool,
    pub selection: alt_remote_protocol::ReviewSelection,
    pub evidence: alt_task_service::EvidenceAssessment,
    pub eligible: bool,
    pub reason: &'static str,
}
impl Store {
    pub fn inspect_current_integration(
        &mut self,
        credential: &Credential,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        tasks: &VerifiedTaskGraph,
        integration_id: &str,
    ) -> Result<CurrentReviewAssessment> {
        check_selection(work, Some(tasks))?;
        let review = integration_review(work, tasks, integration_id)?;
        work.check_read_integrity()?;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let mut current = snapshot(&mut tx, &self.spec, &work.key.scope.repository_instance_id)?;
        reviewer(&mut tx, &self.spec, credential, &current)?;
        if current.scope != work.key.scope {
            return Err(Error::Unauthorized);
        }
        let row = crate::publication::worker(&mut tx, work)?;
        if row.get::<_, String>("state") != "ready" {
            return Err(Error::Precondition("publication not ready for review"));
        }
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

/// The selected approval was already validated before ref changes. Recheck its
/// time-sensitive authority at the end of the same transaction, after potentially
/// expensive pin/record writes. The original snapshot remains the review target.
pub(crate) fn recheck_current_authority(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    work: &PublicationWork,
    tasks: Option<&VerifiedTaskGraph>,
    current: &RepositorySnapshot,
) -> Result<()> {
    let Some(tasks) = tasks else {
        return Ok(());
    };
    if work.publication.refs.is_empty() {
        return Ok(());
    }
    let row = tx.query_one("SELECT reviewer_credential,expires_at,integration_id FROM alt_current_approvals WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[&work.key.scope.repository_instance_id,&integer(work.key.scope.recovery_epoch)?,&work.key.actor_id,&work.key.request_id])?;
    let time = now(tx)?;
    if unsigned(row.get(1))? <= time {
        return Err(Error::Precondition(
            "integration approval expired during publication",
        ));
    }
    reviewer(tx, spec, &Credential { digest: row.get(0) }, current)?;
    let integration: String = row.get(2);
    let review = integration_review(work, tasks, &integration)?;
    let (_, policy) = collector_policy(tx, &current.scope.repository_instance_id)?;
    assessment(tasks, review, current, &policy, time)?;
    Ok(())
}
