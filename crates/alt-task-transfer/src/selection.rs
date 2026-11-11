use crate::{Draft, Error, Result};
use alt_git_codec::{HashAlgo, ObjectKind};
use alt_remote_protocol::{ContentKind, Limits, Publication, RecordRoot};
use alt_repository_service::{ObjectSource, verify_graph};
use alt_task::Artifact;
use alt_task_service::{
    ArtifactSource, AttachmentOmission, CollectorAttestation, PreparedTaskGraph, RecordKey,
    RecordKind, RecordSelection, SourceCompletion, SourceRecovery, TaskBundle, TaskLimits,
    TaskRecord,
};
use std::{collections::BTreeSet, path::Path};

/// The adapter returns exact source bytes; no completion is synthesized from
/// record presence. Imported history can supply its retained original proof.
pub enum Provenance {
    Completed {
        proof: SourceCompletion,
        raw: Vec<Vec<u8>>,
    },
    Recovered {
        proof: SourceRecovery,
        raw: Vec<Vec<u8>>,
    },
}
pub trait Source: ArtifactSource + ObjectSource {
    fn record(&self, key: &RecordKey, maximum: usize) -> Result<Vec<u8>>;
    fn provenance(
        &self,
        key: &RecordKey,
        record: &TaskRecord,
        maximum: usize,
    ) -> Result<Provenance>;
}
#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Selection {
    pub roots: Vec<RecordRoot>,
    pub companions: Vec<RecordKey>,
    pub omissions: Vec<AttachmentOmission>,
    pub attestations: Vec<CollectorAttestation>,
}
pub fn root_key(root: &RecordRoot) -> RecordKey {
    use alt_remote_protocol::RecordKind as K;
    RecordKey {
        id: root.id.clone(),
        kind: match root.kind {
            K::Task => RecordKind::Task,
            K::Candidate => RecordKind::Candidate,
            K::Evidence => RecordKind::Evidence,
            K::Review => RecordKind::Review,
            K::Integration => RecordKind::Integration,
            K::Delivery => RecordKind::Delivery,
        },
    }
}
fn artifact(entry: &alt_remote_protocol::ContentEntry) -> Artifact {
    Artifact {
        blake3: entry.blake3.clone(),
        bytes: entry.bytes,
    }
}
/// Follow only explicit roots/companions and their required dependencies.
/// Omission declarations remain in the bundle and never rewrite a record ID.
pub fn build(
    parent: &Path,
    source: &impl Source,
    selection: Selection,
    mut publication: Publication,
    limits: Limits,
    task_limits: TaskLimits,
) -> Result<(Draft, Publication)> {
    let mut draft = Draft::new(parent, limits)?;
    let mut bundle = TaskBundle {
        schema_version: 1,
        roots: selection.roots,
        companions: selection.companions,
        records: vec![],
        sources: vec![],
        recoveries: vec![],
        omissions: selection.omissions,
        attestations: selection.attestations,
    };
    // Validate inventory limits before traversing any local source.
    if bundle.roots.len() > limits.record_roots
        || bundle.companions.len() > task_limits.records
        || bundle.omissions.len() > task_limits.edges
        || bundle.attestations.len() > task_limits.records
    {
        return Err(Error::Limit("task selection"));
    }
    let mut seen = BTreeSet::new();
    let mut pending: Vec<_> = bundle.roots.iter().map(root_key).collect();
    pending.extend(bundle.companions.clone());
    let mut edges = 0usize;
    while let Some(key) = pending.pop() {
        if !seen.insert(key.clone()) {
            continue;
        }
        if seen.len() > task_limits.records {
            return Err(Error::Limit("task records"));
        }
        let bytes = source.record(&key, task_limits.structured_bytes)?;
        let record = TaskRecord::decode(&key, &bytes, task_limits.structured_bytes)?;
        let entry = draft.capture_bytes(ContentKind::Record, None, &bytes)?;
        bundle.records.push(RecordSelection {
            record: key.clone(),
            payload: artifact(&entry),
        });
        let dependencies = record.dependencies();
        edges = edges
            .checked_add(dependencies.len())
            .ok_or(Error::Limit("task edges"))?;
        if edges > task_limits.edges {
            return Err(Error::Limit("task edges"));
        }
        pending.extend(dependencies);
        if record.creator_request().is_some() {
            let raw = match source.provenance(&key, &record, task_limits.structured_bytes)? {
                Provenance::Completed { proof, raw } => {
                    bundle.sources.push(proof);
                    raw
                }
                Provenance::Recovered { proof, raw } => {
                    bundle.recoveries.push(proof);
                    raw
                }
            };
            if raw.len() > 4 {
                return Err(Error::Limit("source proof artifacts"));
            }
            for bytes in raw {
                if bytes.len() > task_limits.structured_bytes {
                    return Err(Error::Limit("source proof bytes"));
                }
                let digest = alt_hash::blake3::hash(&bytes).to_hex().to_string();
                if !draft
                    .entries()
                    .iter()
                    .any(|e| e.kind == ContentKind::Attachment && e.blake3 == digest)
                {
                    draft.capture_bytes(ContentKind::Attachment, None, &bytes)?;
                }
            }
        }
        for required in record.artifacts() {
            if bundle
                .omissions
                .iter()
                .any(|omission| omission.owner == key && omission.artifact == required)
            {
                continue;
            }
            // A shared artifact is captured once. A distinct semantic record
            // role still receives its own manifest entry.
            if draft.entries().iter().any(|e| {
                e.kind == ContentKind::Attachment
                    && e.blake3 == required.blake3
                    && e.bytes == required.bytes
            }) {
                continue;
            }
            let entry = draft.capture(
                ContentKind::Attachment,
                None,
                required.bytes,
                &mut ArtifactSource::open(source, &required)?,
            )?;
            if artifact(&entry) != required {
                return Err(Error::Invalid("source artifact identity"));
            }
        }
    }
    let prepared = PreparedTaskGraph::load(bundle.clone(), &bundle.roots, &draft, task_limits)?;
    prepared.inspect_sources_and_attachments(&draft)?;
    let mut roots = prepared.object_roots(&draft)?;
    for oid in publication
        .refs
        .iter()
        .flat_map(|r| [&r.expected, &r.new])
        .flatten()
    {
        roots.push(
            oid.parse()
                .map_err(|_| Error::Invalid("publication Git object"))?,
        );
    }
    roots.sort_unstable();
    roots.dedup();
    let graph = verify_graph(source, HashAlgo::Sha1, &roots, limits.into())?;
    for oid in graph.objects() {
        let mut object =
            ObjectSource::open(source, oid)?.ok_or(Error::Invalid("source object missing"))?;
        let kind = match object.kind {
            ObjectKind::Blob => ContentKind::Blob,
            ObjectKind::Tree => ContentKind::Tree,
            ObjectKind::Commit => ContentKind::Commit,
            ObjectKind::Tag => ContentKind::Tag,
        };
        draft.capture(
            kind,
            Some(oid.to_string()),
            object.bytes,
            &mut object.reader,
        )?;
    }
    draft.capture_bytes(ContentKind::TaskBundle, None, &alt_json::to_vec(&bundle)?)?;
    publication.record_roots = bundle.roots;
    publication.content_manifest = draft.manifest()?.id(limits)?;
    draft
        .manifest()?
        .validate_publication(&publication, limits)?;
    Ok((draft, publication))
}
