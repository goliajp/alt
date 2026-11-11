use crate::{
    ArtifactSource, CollectorPolicy, CollectorTrust, Error, PreparedTaskGraph, RecordKey,
    RecordKind, Result, TaskRecord,
};
use alt_repository_service::{ObjectSource, VerifiedGraph};
use std::collections::BTreeMap;

#[derive(Clone, Debug, alt_json::ToJson)]
pub struct EvidenceAssessment {
    pub evidence: String,
    pub source_proof: String,
    pub eligible: bool,
    pub reasons: Vec<String>,
    pub collectors: BTreeMap<String, CollectorTrust>,
}
impl PreparedTaskGraph {
    /// Derive a current decision from immutable received history and an
    /// adapter-provided current ref/policy snapshot. No mutation capability is
    /// returned; the transaction must repeat mutable authorization checks.
    #[allow(clippy::too_many_arguments)]
    pub fn assess_evidence(
        &self,
        evidence_id: &str,
        graph: &VerifiedGraph,
        objects: &impl ObjectSource,
        artifacts: &impl ArtifactSource,
        current_target: &BTreeMap<String, String>,
        collectors: &CollectorPolicy,
        now_unix_ms: u64,
    ) -> Result<EvidenceAssessment> {
        let evidence_key = RecordKey {
            kind: RecordKind::Evidence,
            id: evidence_id.into(),
        };
        let TaskRecord::Evidence(evidence) = self.record(&evidence_key)? else {
            return Err(Error::Integrity("evidence kind"));
        };
        let TaskRecord::Candidate(candidate) = self.record(&RecordKey {
            kind: RecordKind::Candidate,
            id: evidence.candidate.clone(),
        })?
        else {
            return Err(Error::Integrity("evidence candidate kind"));
        };
        let TaskRecord::Task(task) = self.record(&RecordKey {
            kind: RecordKind::Task,
            id: candidate.task.clone(),
        })?
        else {
            return Err(Error::Integrity("evidence task kind"));
        };
        let TaskRecord::Manifest(manifest) = self.record(&RecordKey {
            kind: RecordKind::Manifest,
            id: candidate.manifest.clone(),
        })?
        else {
            return Err(Error::Integrity("evidence candidate manifest kind"));
        };
        crate::verify_candidate_inputs(candidate, task, manifest, graph, objects, self.limits)?;
        let facts = self.inspect_sources_and_attachments(artifacts)?;
        self.assess_checked_evidence(evidence_id, &facts, current_target, collectors, now_unix_ms)
    }
    pub(crate) fn assess_checked_evidence(
        &self,
        evidence_id: &str,
        facts: &crate::ReceivedFacts,
        current_target: &BTreeMap<String, String>,
        collectors: &CollectorPolicy,
        now_unix_ms: u64,
    ) -> Result<EvidenceAssessment> {
        let evidence_key = RecordKey {
            kind: RecordKind::Evidence,
            id: evidence_id.into(),
        };
        let TaskRecord::Evidence(evidence) = self.record(&evidence_key)? else {
            return Err(Error::Integrity("evidence kind"));
        };
        let TaskRecord::Candidate(candidate) = self.record(&RecordKey {
            kind: RecordKind::Candidate,
            id: evidence.candidate.clone(),
        })?
        else {
            return Err(Error::Integrity("evidence candidate kind"));
        };
        let TaskRecord::Manifest(manifest) = self.record(&RecordKey {
            kind: RecordKind::Manifest,
            id: candidate.manifest.clone(),
        })?
        else {
            return Err(Error::Integrity("evidence candidate manifest kind"));
        };
        let evidence_facts = facts
            .records
            .iter()
            .find(|facts| facts.record == evidence_key)
            .ok_or(Error::Integrity("evidence source facts"))?;
        let source_proof = evidence_facts
            .source_proof
            .clone()
            .ok_or(Error::Integrity("evidence source proof"))?;
        let mut validity = alt_task::evaluate(
            candidate,
            manifest,
            evidence,
            &candidate.manifest,
            current_target,
            |artifact| {
                if evidence_facts.omitted.contains(artifact) {
                    return Err(alt_task::Error::Invalid("attachment omitted".into()));
                }
                if !facts.retained_artifacts().contains(artifact) {
                    return Err(alt_task::Error::Invalid("attachment not verified".into()));
                }
                Ok(())
            },
        )?;
        if !evidence_facts.omitted.is_empty()
            || facts.records.iter().any(|facts| {
                facts.record.kind == RecordKind::Manifest
                    && !facts.omitted.is_empty()
                    && (facts.record.id == candidate.manifest
                        || evidence.before.as_ref() == Some(&facts.record.id)
                        || evidence.after.as_ref() == Some(&facts.record.id))
            })
        {
            validity.reasons.push("attachments_omitted".into());
        }
        collectors.validate()?;
        let mut decisions = BTreeMap::new();
        for attestation in self
            .bundle
            .attestations
            .iter()
            .filter(|a| a.evidence == evidence_id)
        {
            if attestation.source_proof != source_proof {
                return Err(Error::Integrity("evidence attestation source binding"));
            }
            decisions.insert(
                attestation.signing_key_id.clone(),
                crate::verify_collector(attestation, &evidence.runner, collectors, now_unix_ms)?,
            );
        }
        if !decisions
            .values()
            .any(|status| *status == CollectorTrust::Trusted)
        {
            validity.reasons.push("collector_not_trusted".into());
        }
        validity.reasons.sort();
        validity.reasons.dedup();
        Ok(EvidenceAssessment {
            evidence: evidence_id.into(),
            source_proof,
            eligible: validity.reasons.is_empty(),
            reasons: validity.reasons,
            collectors: decisions,
        })
    }
}
