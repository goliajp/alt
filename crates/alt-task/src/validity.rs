use crate::{Artifact, Candidate, Evidence, Provenance, Record, Result, Termination};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
pub struct Validity {
    pub eligible: bool,
    pub reasons: Vec<String>,
}

/// Derive validity afresh, without rewriting the original receipt. A caller
/// must also verify task/request completion and target preconditions at the
/// integration transaction; this predicate grants no mutation capability.
pub fn evaluate(
    candidate: &Candidate,
    manifest: &alt_input::Manifest,
    evidence: &Evidence,
    current_manifest: &str,
    current_target: &BTreeMap<String, String>,
    mut verify: impl FnMut(&Artifact) -> Result<()>,
) -> Result<Validity> {
    candidate.validate()?;
    evidence.validate()?;
    let manifest_id = manifest.id()?;
    let mut reasons = Vec::new();
    if candidate.id()? != evidence.candidate {
        reasons.push("candidate_mismatch".into());
    }
    if candidate.manifest != manifest_id || evidence.manifest != manifest_id {
        reasons.push("manifest_mismatch".into());
    }
    if current_manifest != manifest_id {
        reasons.push("inputs_stale".into());
    }
    if current_target != &candidate.target_expected {
        reasons.push("target_changed".into());
    }
    if evidence.provenance != Provenance::Observed {
        reasons.push("external_report_unobserved".into());
    }
    if evidence.before.as_deref() != Some(&manifest_id) {
        reasons.push("input_before_mismatch_or_unknown".into());
    }
    if evidence.after.as_deref() != Some(&manifest_id) {
        reasons.push("input_after_mismatch_or_unknown".into());
    }
    if evidence.actual_context != manifest.context {
        reasons.push("execution_context_mismatch".into());
    }
    reasons.extend(evidence.actual_context.uncertainty());
    if evidence.runner.is_empty() {
        reasons.push("runner_unknown".into());
    }
    if evidence.started_unix_ns == 0 || evidence.finished_unix_ns < evidence.started_unix_ns {
        reasons.push("execution_times_unknown".into());
    }
    match evidence.termination {
        Termination::Exit { code: 0 } => {}
        Termination::Exit { .. } => reasons.push("validation_failed".into()),
        Termination::Signal { .. } => reasons.push("terminated_by_signal".into()),
        Termination::Timeout => reasons.push("timed_out".into()),
        Termination::Unknown => reasons.push("exit_unknown".into()),
    }
    for (name, log) in [("stdout", &evidence.stdout), ("stderr", &evidence.stderr)] {
        match log {
            None => reasons.push(format!("{name}_missing")),
            Some(log) => {
                if verify(log).is_err() {
                    reasons.push(format!("{name}_corrupt_or_missing"));
                }
            }
        }
    }
    if evidence.binaries.is_empty() {
        reasons.push("executed_binaries_missing".into());
    }
    for tool in &evidence.actual_context.tools {
        if evidence
            .binaries
            .get(&tool.name)
            .map(|artifact| artifact.blake3.as_str())
            != tool.binary_blake3.as_deref()
        {
            reasons.push(format!("tool_binary_mismatch:{}", tool.name));
        }
    }
    for (name, artifact) in evidence.binaries.iter().chain(&evidence.artifacts) {
        if verify(artifact).is_err() {
            reasons.push(format!("artifact_corrupt_or_missing:{name}"));
        }
    }
    reasons.sort();
    reasons.dedup();
    Ok(Validity {
        eligible: reasons.is_empty(),
        reasons,
    })
}
