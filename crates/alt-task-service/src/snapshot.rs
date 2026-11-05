use crate::{Error, Result, TaskLimits, inputs::oid};
use alt_repository_service::{Node, ObjectSource, VerifiedGraph};

/// Validate the original public inventory against pinned bytes. Snapshot refs
/// describe its historical source and do not grant publication authority.
pub fn verify_baseline_snapshot(
    snapshot: &alt_delivery::Snapshot,
    graph: &VerifiedGraph,
    source: &impl ObjectSource,
    limits: TaskLimits,
) -> Result<()> {
    verify_baseline_snapshot_bounded(snapshot, graph, source, limits, &mut { limits.walk_steps })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_baseline_snapshot_bounded(
    snapshot: &alt_delivery::Snapshot,
    graph: &VerifiedGraph,
    source: &impl ObjectSource,
    limits: TaskLimits,
    remaining: &mut usize,
) -> Result<()> {
    if snapshot.schema_version != 1
        || snapshot.object_format != "sha1"
        || !snapshot.refs.contains_key("HEAD")
    {
        return Err(Error::Integrity("baseline snapshot header"));
    }
    if snapshot.objects.len() > limits.objects || snapshot.refs.len() > limits.edges {
        return Err(Error::Limit("baseline inventory"));
    }
    alt_delivery::validate_config(&alt_input::unhex(&snapshot.config_hex)?)
        .map_err(|_| Error::Integrity("baseline config"))?;
    let mut total = 0u64;
    let mut edges = 0usize;
    for (id, expected) in &snapshot.objects {
        crate::verified::step(remaining, 1)?;
        let id = oid(id)?;
        total = total
            .checked_add(expected.bytes)
            .ok_or(Error::Limit("baseline bytes"))?;
        if total > limits.total_bytes {
            return Err(Error::Limit("baseline bytes"));
        }
        if crate::inputs::object_identity(source, id, limits)? != *expected {
            return Err(Error::Integrity("baseline object identity"));
        }
        let dependencies: Vec<_> = match graph.node(id)? {
            Node::Blob => vec![],
            Node::Tree(entries) => entries
                .iter()
                .filter(|e| e.mode != 0o160000)
                .map(|e| e.oid)
                .collect(),
            Node::Commit { tree, parents } => std::iter::once(*tree)
                .chain(parents.iter().copied())
                .collect(),
            Node::Tag { target, .. } => vec![*target],
        };
        crate::verified::step(remaining, dependencies.len())?;
        edges = edges
            .checked_add(dependencies.len())
            .ok_or(Error::Limit("baseline edges"))?;
        if edges > limits.edges {
            return Err(Error::Limit("baseline edges"));
        }
        if dependencies
            .iter()
            .any(|id| !snapshot.objects.contains_key(&id.to_string()))
        {
            return Err(Error::Integrity(
                "baseline closure leaves original inventory",
            ));
        }
    }
    for name in snapshot.refs.keys() {
        crate::verified::step(remaining, snapshot.refs.len())?;
        alt_input::checked_path(name.as_bytes())?;
        if name != "HEAD" && !alt_delivery::public_ref(name) {
            return Err(Error::Integrity("private baseline ref"));
        }
        let id = alt_delivery::resolve(&snapshot.refs, name)
            .map_err(|_| Error::Integrity("baseline ref resolution"))?;
        if !snapshot.objects.contains_key(&id.to_string()) {
            return Err(Error::Integrity("baseline ref outside inventory"));
        }
    }
    Ok(())
}
