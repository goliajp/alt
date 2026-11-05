use crate::{
    ArtifactSource, Error, Result, TaskLimits,
    inputs::{oid, tree_files},
};
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_repository_service::VerifiedGraph;
use alt_task::Record;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Changes {
    changes: Vec<Change>,
    truncated: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    path: String,
    path_bytes_hex: String,
    before: Option<Value>,
    after: Option<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Value {
    mode: u32,
    oid: String,
    bytes_hex: String,
    utf8: Option<String>,
}
fn verify_value(value: &Option<Value>, expected: Option<&(u32, ObjectId)>) -> Result<()> {
    match (value, expected) {
        (None, None) => Ok(()),
        (Some(value), Some((mode, id))) => {
            let bytes = alt_input::unhex(&value.bytes_hex)?;
            if value.mode != *mode
                || oid(&value.oid)? != *id
                || ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &bytes) != *id
                || value.utf8.as_deref() != std::str::from_utf8(&bytes).ok()
            {
                return Err(Error::Integrity("review before/after bytes"));
            }
            Ok(())
        }
        _ => Err(Error::Integrity("review before/after existence")),
    }
}
/// Validate the full original review diff, including binary bytes and modes.
/// This historical record does not count as a current server approval.
pub fn verify_review_changes(
    review: &alt_task::Review,
    task: &alt_task::Task,
    candidate: &alt_task::Candidate,
    graph: &VerifiedGraph,
    source: &impl ArtifactSource,
    limits: TaskLimits,
) -> Result<()> {
    verify_review_changes_bounded(review, task, candidate, graph, source, limits, &mut {
        limits.walk_steps
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_review_changes_bounded(
    review: &alt_task::Review,
    task: &alt_task::Task,
    candidate: &alt_task::Candidate,
    graph: &VerifiedGraph,
    source: &impl ArtifactSource,
    limits: TaskLimits,
    remaining: &mut usize,
) -> Result<()> {
    if review.task != task.id()?
        || review.candidate != candidate.id()?
        || candidate.task != review.task
        || review.target_expected != candidate.target_expected
    {
        return Err(Error::Integrity("review input identity"));
    }
    let raw = crate::source::read_artifact(source, &review.changes, limits.structured_bytes)?;
    let changes: Changes =
        serde_json::from_value(crate::strict_json::decode(&raw, limits.structured_bytes)?)?;
    if changes.truncated {
        return Err(Error::Integrity("truncated original review"));
    }
    let before = tree_files(graph, oid(&task.baseline.tree)?, limits, remaining)?;
    let after = tree_files(graph, oid(&candidate.tree)?, limits, remaining)?;
    let expected: BTreeSet<_> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect();
    let mut seen = BTreeSet::new();
    for change in changes.changes {
        if alt_input::unhex(&change.path_bytes_hex)? != change.path.as_bytes()
            || !expected.contains(&change.path)
            || !seen.insert(change.path.clone())
        {
            return Err(Error::Integrity("review changed path set"));
        }
        verify_value(&change.before, before.get(&change.path))?;
        verify_value(&change.after, after.get(&change.path))?;
    }
    if expected != seen {
        return Err(Error::Integrity("review omitted changed paths"));
    }
    Ok(())
}
