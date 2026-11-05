use crate::{
    Error, Result, TaskLimits,
    inputs::{oid, tree_files},
};
use alt_repository_service::{Node, ObjectSource, VerifiedGraph};
use std::collections::{BTreeMap, BTreeSet};

pub fn verify_git_check(check: &alt_delivery::GitCheck) -> Result<()> {
    alt_task::check_id(&check.binary_blake3)?;
    if !std::path::Path::new(&check.program).is_absolute() || check.fsck_exit != 0 {
        return Err(Error::Integrity("original Git verification result"));
    }
    for value in [
        &check.version_stdout_hex,
        &check.version_stderr_hex,
        &check.fsck_stdout_hex,
        &check.fsck_stderr_hex,
    ] {
        alt_input::unhex(value)?;
    }
    if check.version_stdout_hex.is_empty() {
        return Err(Error::Integrity("original Git version missing"));
    }
    Ok(())
}
/// Verify the preserved source delivery report. This proves the report's
/// internal binding and bytes; it is not a fresh execution of Git or a claim
/// that the source destination still exists.
pub fn verify_delivery_report(
    report: &alt_delivery::ExportReport,
    baseline: &alt_delivery::Snapshot,
    integration: &alt_task::Integration,
    graph: &VerifiedGraph,
    source: &impl ObjectSource,
    limits: TaskLimits,
) -> Result<()> {
    verify_delivery_report_bounded(report, baseline, integration, graph, source, limits, &mut {
        limits.walk_steps
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_delivery_report_bounded(
    report: &alt_delivery::ExportReport,
    baseline: &alt_delivery::Snapshot,
    integration: &alt_task::Integration,
    graph: &VerifiedGraph,
    source: &impl ObjectSource,
    limits: TaskLimits,
    remaining: &mut usize,
) -> Result<()> {
    if report.baseline_id
        != baseline
            .id()
            .map_err(|_| Error::Integrity("baseline identity"))?
        || report.config_transform != "files refs; extensions.refstorage removed"
    {
        return Err(Error::Integrity("delivery report baseline or transform"));
    }
    crate::snapshot::verify_baseline_snapshot_bounded(baseline, graph, source, limits, remaining)?;
    crate::snapshot::verify_baseline_snapshot_bounded(
        &report.exported,
        graph,
        source,
        limits,
        remaining,
    )?;
    let mut expected_refs = baseline.refs.clone();
    expected_refs.insert(integration.target.clone(), integration.commit.clone());
    expected_refs.insert(
        "refs/notes/alt/meta".into(),
        integration.notes_after.clone(),
    );
    if report.exported.refs != expected_refs {
        return Err(Error::Integrity("delivery ref selection"));
    }
    let original = alt_git_config::parse_file(&alt_input::unhex(&baseline.config_hex)?)
        .map_err(|_| Error::Integrity("delivery original config"))?;
    let expected: Vec<_> = original
        .into_iter()
        .filter(|e| e.section != "extensions" || e.key != "refstorage")
        .collect();
    let actual = alt_git_config::parse_file(&alt_input::unhex(&report.exported.config_hex)?)
        .map_err(|_| Error::Integrity("delivery exported config"))?;
    if actual != expected {
        return Err(Error::Integrity("delivery config semantics"));
    }
    let mut expected_objects: BTreeSet<_> = baseline.objects.keys().cloned().collect();
    let mut roots = vec![oid(&integration.commit)?];
    for name in expected_refs.keys() {
        roots.push(
            alt_delivery::resolve(&expected_refs, name)
                .map_err(|_| Error::Integrity("delivery ref resolution"))?,
        );
    }
    for root in roots {
        expected_objects.extend(
            crate::verified::reachable(graph, root, remaining)?
                .into_iter()
                .map(|id| id.to_string()),
        );
        if expected_objects.len() > limits.objects {
            return Err(Error::Limit("delivery objects"));
        }
    }
    if expected_objects != report.exported.objects.keys().cloned().collect() {
        return Err(Error::Integrity("delivery object selection"));
    }
    for (id, original) in &baseline.objects {
        if report.exported.objects.get(id) != Some(original) {
            return Err(Error::Integrity("delivery original object changed"));
        }
    }
    let head = alt_delivery::resolve(&expected_refs, "HEAD")
        .map_err(|_| Error::Integrity("delivery HEAD"))?;
    let Node::Commit { tree, .. } = graph.node(head)? else {
        return Err(Error::Integrity("delivery HEAD commit type"));
    };
    let files = tree_files(graph, *tree, limits, remaining)?;
    let expected_files: BTreeMap<_, _> = files
        .into_iter()
        .map(|(path, (mode, id))| {
            (
                path,
                alt_delivery::FileEntry {
                    mode,
                    oid: id.to_string(),
                },
            )
        })
        .collect();
    if report.files != expected_files {
        return Err(Error::Integrity("delivery checkout file set"));
    }
    verify_git_check(&report.git)
}
