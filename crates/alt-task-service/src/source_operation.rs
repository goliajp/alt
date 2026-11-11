use crate::{ArtifactSource, Error, Result, SourceCompletion};
use alt_task::Record;

#[derive(alt_json::FromJson)]
struct Context {
    version: u32,
    workspace: Workspace,
    request_id: Option<String>,
}
#[derive(alt_json::FromJson)]
struct Workspace {
    instance: String,
}
#[derive(Clone, Debug)]
pub struct SourceOperation {
    pub id: String,
    pub actor: String,
    pub timestamp_ms: u64,
}
/// Validate the original attributed transaction as source history. It grants
/// no authority to execute that transaction in the receiving repository.
pub fn verify_integration_operation(
    integration: &alt_task::Integration,
    proof: &SourceCompletion,
    completed_operations: &[String],
    source: &impl ArtifactSource,
    limit: usize,
) -> Result<SourceOperation> {
    let artifact = proof
        .ref_operation
        .as_ref()
        .ok_or(Error::Integrity("integration operation missing"))?;
    if proof.record.kind != crate::RecordKind::Integration
        || proof.record.id != integration.id()?
        || integration.creator_request != proof.request_id
    {
        return Err(Error::Integrity("source operation record binding"));
    }
    let checked = verify_attributed_operation(integration, &proof.origin, artifact, source, limit)?;
    if !completed_operations.contains(&checked.id) {
        return Err(Error::Integrity("source operation completion binding"));
    }
    Ok(checked)
}

pub(crate) fn verify_attributed_operation(
    integration: &alt_task::Integration,
    origin: &alt_task::Scope,
    artifact: &alt_task::Artifact,
    source: &impl ArtifactSource,
    limit: usize,
) -> Result<SourceOperation> {
    let raw = crate::source::read_artifact(source, artifact, limit)?;
    let op =
        alt_oplog::Op::from_body(&raw).map_err(|_| Error::Integrity("source operation body"))?;
    let id = op.id.to_string();
    let tx = alt_refs::parse_tx(&op.payload)
        .map_err(|_| Error::Integrity("source ref transaction"))?
        .ok_or(Error::Integrity("source operation is not ref transaction"))?;
    let context = tx
        .context
        .ok_or(Error::Integrity("source operation attribution missing"))?;
    let context: Context = alt_json::from_value(crate::strict_json::decode(&context, limit)?)?;
    if !matches!(context.version, 1 | 2)
        || context.workspace.instance != origin.workspace
        || context.request_id.as_deref() != Some(integration.creator_request.as_str())
        || tx.changes.len() != 2
    {
        return Err(Error::Integrity("source operation attribution"));
    }
    let branch = tx
        .changes
        .iter()
        .find(|change| change.name == integration.target);
    let notes = tx
        .changes
        .iter()
        .find(|change| change.name == "refs/notes/alt/meta");
    if !branch.is_some_and(|change| {
        change.old.as_ref().map(alt_delivery::render).as_deref() == Some(&integration.before)
            && change.new.as_ref().map(alt_delivery::render).as_deref() == Some(&integration.commit)
    }) || !notes.is_some_and(|change| {
        change.old.as_ref().map(alt_delivery::render) == integration.notes_before
            && change.new.as_ref().map(alt_delivery::render).as_deref()
                == Some(&integration.notes_after)
    }) {
        return Err(Error::Integrity("source integration transaction changes"));
    }
    Ok(SourceOperation {
        id,
        actor: op.actor,
        timestamp_ms: op.timestamp_ms,
    })
}
