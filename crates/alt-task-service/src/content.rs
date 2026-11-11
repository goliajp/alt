use crate::{
    ArtifactSource, Error, PreparedTaskGraph, RecordKey, RecordKind, Result, TaskRecord,
    inputs::oid,
};
use alt_git_codec::ObjectKind;
use alt_repository_service::{Node, ObjectSource, VerifiedGraph};
use std::io::Read;

impl PreparedTaskGraph {
    /// Verify selected records' canonical object bindings. This is one part of
    /// received-history validation; source operations and current acceptance
    /// still require their separate checks.
    pub fn verify_object_bindings(
        &self,
        graph: &VerifiedGraph,
        objects: &impl ObjectSource,
        artifacts: &impl ArtifactSource,
    ) -> Result<()> {
        self.verify_object_bindings_bounded(graph, objects, artifacts, &mut {
            self.limits.walk_steps
        })
    }
    pub(crate) fn verify_object_bindings_bounded(
        &self,
        graph: &VerifiedGraph,
        objects: &impl ObjectSource,
        artifacts: &impl ArtifactSource,
        remaining: &mut usize,
    ) -> Result<()> {
        let lookup = |kind, id: &str| {
            self.record(&RecordKey {
                kind,
                id: id.into(),
            })
        };
        for record in self.records.values() {
            match record {
                TaskRecord::Task(task) => {
                    let Node::Commit { tree, .. } = graph.node(oid(&task.baseline.head)?)? else {
                        return Err(Error::Integrity("task baseline commit type"));
                    };
                    if *tree != oid(&task.baseline.tree)? {
                        return Err(Error::Integrity("task baseline commit tree"));
                    }
                    for target in task.target_expected.values() {
                        if !matches!(graph.node(oid(target)?)?, Node::Commit { .. }) {
                            return Err(Error::Integrity("task target commit type"));
                        }
                    }
                }
                TaskRecord::Candidate(candidate) => {
                    let TaskRecord::Task(task) = lookup(RecordKind::Task, &candidate.task)? else {
                        return Err(Error::Integrity("candidate task kind"));
                    };
                    let TaskRecord::Manifest(manifest) =
                        lookup(RecordKind::Manifest, &candidate.manifest)?
                    else {
                        return Err(Error::Integrity("candidate manifest kind"));
                    };
                    crate::inputs::verify_candidate_inputs_bounded(
                        candidate,
                        task,
                        manifest,
                        graph,
                        objects,
                        self.limits,
                        remaining,
                    )?;
                }
                TaskRecord::TaskBaseline(baseline) => {
                    let raw = crate::source::read_artifact(
                        artifacts,
                        &baseline.snapshot,
                        self.limits.structured_bytes,
                    )?;
                    let snapshot: alt_delivery::Snapshot = alt_json::from_value(
                        crate::strict_json::decode(&raw, self.limits.structured_bytes)?,
                    )?;
                    crate::snapshot::verify_baseline_snapshot_bounded(
                        &snapshot,
                        graph,
                        objects,
                        self.limits,
                        remaining,
                    )?;
                }
                TaskRecord::Review(review) => {
                    let TaskRecord::Task(task) = lookup(RecordKind::Task, &review.task)? else {
                        return Err(Error::Integrity("review task kind"));
                    };
                    let TaskRecord::Candidate(candidate) =
                        lookup(RecordKind::Candidate, &review.candidate)?
                    else {
                        return Err(Error::Integrity("review candidate kind"));
                    };
                    crate::review::verify_review_changes_bounded(
                        review,
                        task,
                        candidate,
                        graph,
                        artifacts,
                        self.limits,
                        remaining,
                    )?;
                }
                TaskRecord::Integration(integration) => {
                    let TaskRecord::Review(review) =
                        lookup(RecordKind::Review, &integration.review)?
                    else {
                        return Err(Error::Integrity("integration review kind"));
                    };
                    let TaskRecord::Candidate(candidate) =
                        lookup(RecordKind::Candidate, &review.candidate)?
                    else {
                        return Err(Error::Integrity("integration candidate kind"));
                    };
                    let commit_id = oid(&integration.commit)?;
                    let Node::Commit { tree, parents } = graph.node(commit_id)? else {
                        return Err(Error::Integrity("integration commit type"));
                    };
                    if *tree != oid(&candidate.tree)?
                        || parents.as_slice() != [oid(&integration.before)?]
                    {
                        return Err(Error::Integrity("integration commit candidate or parent"));
                    }
                    let object = objects
                        .open(commit_id)?
                        .ok_or(Error::Integrity("integration commit missing"))?;
                    if object.kind != ObjectKind::Commit
                        || object.bytes > self.limits.structured_bytes as u64
                    {
                        return Err(Error::Limit("integration commit bytes"));
                    }
                    let mut raw = Vec::new();
                    let bytes = object.bytes;
                    object.reader.take(bytes + 1).read_to_end(&mut raw)?;
                    if raw.len() as u64 != bytes
                        || alt_git_codec::ObjectId::hash_object(
                            commit_id.algo(),
                            ObjectKind::Commit,
                            &raw,
                        ) != commit_id
                    {
                        return Err(Error::Integrity("integration commit bytes"));
                    }
                    let parsed = alt_git_codec::Commit::parse(&raw)
                        .map_err(|_| Error::Integrity("integration commit encoding"))?;
                    let mut message = review.commit_message.clone();
                    if !message.ends_with('\n') {
                        message.push('\n');
                    }
                    if parsed.message().as_slice() != message.as_bytes() {
                        return Err(Error::Integrity("integration commit review message"));
                    }
                    if !matches!(
                        graph.node(oid(&integration.notes_after)?)?,
                        Node::Commit { .. }
                    ) {
                        return Err(Error::Integrity("integration notes commit type"));
                    }
                    if let Some(notes) = &integration.notes_before
                        && !matches!(graph.node(oid(notes)?)?, Node::Commit { .. })
                    {
                        return Err(Error::Integrity("integration original notes type"));
                    }
                }
                TaskRecord::Delivery(delivery) => {
                    let TaskRecord::Integration(integration) =
                        lookup(RecordKind::Integration, &delivery.integration)?
                    else {
                        return Err(Error::Integrity("delivery integration kind"));
                    };
                    let TaskRecord::Review(review) =
                        lookup(RecordKind::Review, &integration.review)?
                    else {
                        return Err(Error::Integrity("delivery review kind"));
                    };
                    let TaskRecord::TaskBaseline(baseline) =
                        lookup(RecordKind::TaskBaseline, &review.baseline)?
                    else {
                        return Err(Error::Integrity("delivery baseline kind"));
                    };
                    let raw = crate::source::read_artifact(
                        artifacts,
                        &baseline.snapshot,
                        self.limits.structured_bytes,
                    )?;
                    let snapshot: alt_delivery::Snapshot = alt_json::from_value(
                        crate::strict_json::decode(&raw, self.limits.structured_bytes)?,
                    )?;
                    let raw = crate::source::read_artifact(
                        artifacts,
                        &delivery.report,
                        self.limits.structured_bytes,
                    )?;
                    let report: alt_delivery::ExportReport = alt_json::from_value(
                        crate::strict_json::decode(&raw, self.limits.structured_bytes)?,
                    )?;
                    crate::delivery::verify_delivery_report_bounded(
                        &report,
                        &snapshot,
                        integration,
                        graph,
                        objects,
                        self.limits,
                        remaining,
                    )?;
                }
                TaskRecord::DeliveryReceipt(receipt) => {
                    let raw = crate::source::read_artifact(
                        artifacts,
                        &receipt.verification,
                        self.limits.structured_bytes,
                    )?;
                    let check: alt_delivery::GitCheck = alt_json::from_value(
                        crate::strict_json::decode(&raw, self.limits.structured_bytes)?,
                    )?;
                    crate::verify_git_check(&check)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}
