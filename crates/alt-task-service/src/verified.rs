use crate::{
    ArtifactSource, Error, PreparedTaskGraph, ReceivedFacts, Result, TaskBundle, TaskLimits,
};
use alt_git_codec::ObjectId;
use alt_remote_protocol::{ContentKind, Manifest, Publication};
use alt_repository_service::{Node, ObjectReader, ObjectSource, VerifiedGraph};
use alt_task::Artifact;
use std::{cell::Cell, collections::BTreeSet, io::Read};

pub(crate) fn step(remaining: &mut usize, count: usize) -> Result<()> {
    *remaining = remaining
        .checked_sub(count)
        .ok_or(Error::Limit("task validation work"))?;
    Ok(())
}
pub(crate) fn reachable(
    graph: &VerifiedGraph,
    root: ObjectId,
    remaining: &mut usize,
) -> Result<BTreeSet<ObjectId>> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        step(remaining, 1)?;
        if !seen.insert(id) {
            continue;
        }
        match graph.node(id)? {
            Node::Blob => {}
            Node::Commit { tree, parents } => {
                pending.push(*tree);
                pending.extend(parents);
            }
            Node::Tree(entries) => {
                pending.extend(entries.iter().filter(|e| e.mode != 0o160000).map(|e| e.oid))
            }
            Node::Tag { target, .. } => pending.push(*target),
        }
    }
    Ok(seen)
}
struct Budget<'a, O, A> {
    objects: &'a O,
    artifacts: &'a A,
    bytes: Cell<u64>,
    opens: Cell<usize>,
}
impl<O, A> Budget<'_, O, A> {
    fn charge(&self, bytes: u64) -> Result<()> {
        self.bytes.set(
            self.bytes
                .get()
                .checked_sub(bytes)
                .ok_or(Error::Limit("task verification bytes"))?,
        );
        self.opens.set(
            self.opens
                .get()
                .checked_sub(1)
                .ok_or(Error::Limit("task verification reads"))?,
        );
        Ok(())
    }
}
impl<O: ObjectSource, A> ObjectSource for Budget<'_, O, A> {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        let value = self.objects.open(oid)?;
        self.charge(value.as_ref().map_or(0, |v| v.bytes))
            .map_err(|_| alt_repository_service::Error::Limit("task verification work"))?;
        Ok(value)
    }
}
impl<O, A: ArtifactSource> ArtifactSource for Budget<'_, O, A> {
    fn open(&self, artifact: &Artifact) -> Result<Box<dyn Read + '_>> {
        self.charge(artifact.bytes)?;
        self.artifacts.open(artifact)
    }
}
/// Complete, bounded validation of selected received history. Only this
/// constructor creates the value accepted by the transaction adapter. It is
/// not a current approval and does not authorize executing imported intents.
pub struct VerifiedTaskGraph {
    prepared: PreparedTaskGraph,
    facts: ReceivedFacts,
    bundle: Artifact,
    git_objects: BTreeSet<ObjectId>,
}
impl VerifiedTaskGraph {
    pub fn verify(
        publication: &Publication,
        manifest: &Manifest,
        protocol_limits: alt_remote_protocol::Limits,
        graph: &VerifiedGraph,
        objects: &impl ObjectSource,
        artifacts: &impl ArtifactSource,
        limits: TaskLimits,
    ) -> Result<Self> {
        manifest
            .validate_publication(publication, protocol_limits)
            .map_err(|_| Error::Integrity("task publication manifest"))?;
        let entry = manifest
            .entries
            .iter()
            .find(|e| e.kind == ContentKind::TaskBundle)
            .ok_or(Error::Integrity("task bundle missing"))?;
        let bundle = Artifact {
            blake3: entry.blake3.clone(),
            bytes: entry.bytes,
        };
        let bounded = Budget {
            objects,
            artifacts,
            bytes: Cell::new(limits.work_bytes),
            opens: Cell::new(limits.walk_steps),
        };
        let raw = crate::source::read_artifact(&bounded, &bundle, limits.structured_bytes)?;
        let selection = TaskBundle::decode(&raw, limits)?;
        let prepared =
            PreparedTaskGraph::load(selection, &publication.record_roots, &bounded, limits)?;
        let facts = prepared.inspect_sources_and_attachments(&bounded)?;
        let total = facts
            .retained
            .iter()
            .try_fold(bundle.bytes, |sum, a| sum.checked_add(a.bytes))
            .ok_or(Error::Limit("task retained bytes"))?;
        if total > limits.total_bytes {
            return Err(Error::Limit("task retained bytes"));
        }
        let mut remaining = limits.walk_steps;
        prepared.verify_object_bindings_bounded(graph, &bounded, &bounded, &mut remaining)?;
        let mut git_objects = BTreeSet::new();
        for root in prepared.object_roots(&bounded)? {
            git_objects.extend(reachable(graph, root, &mut remaining)?);
            if git_objects.len() > limits.objects {
                return Err(Error::Limit("task objects"));
            }
        }
        let records: BTreeSet<_> = prepared
            .bundle
            .records
            .iter()
            .map(|r| (&r.payload.blake3, r.payload.bytes))
            .collect();
        let retained: BTreeSet<_> = facts
            .retained
            .iter()
            .map(|a| (&a.blake3, a.bytes))
            .collect();
        let mut selected = BTreeSet::new();
        for entry in &manifest.entries {
            let identity = (&entry.blake3, entry.bytes);
            match entry.kind {
                ContentKind::TaskBundle => {}
                ContentKind::Record if records.contains(&identity) => {
                    selected.insert(identity);
                }
                ContentKind::Attachment if retained.contains(&identity) => {
                    selected.insert(identity);
                }
                ContentKind::Record | ContentKind::Attachment => {
                    return Err(Error::Integrity("unselected task content"));
                }
                _ => {}
            }
        }
        if selected != retained
            || records.iter().any(|id| {
                !manifest
                    .entries
                    .iter()
                    .any(|e| e.kind == ContentKind::Record && (&e.blake3, e.bytes) == *id)
            })
        {
            return Err(Error::Integrity("task content manifest incomplete"));
        }
        Ok(Self {
            prepared,
            facts,
            bundle,
            git_objects,
        })
    }
    pub fn assess_evidence(
        &self,
        evidence_id: &str,
        current_target: &std::collections::BTreeMap<String, String>,
        collectors: &crate::CollectorPolicy,
        now_unix_ms: u64,
    ) -> Result<crate::EvidenceAssessment> {
        self.prepared.assess_checked_evidence(
            evidence_id,
            &self.facts,
            current_target,
            collectors,
            now_unix_ms,
        )
    }
    pub fn prepared(&self) -> &PreparedTaskGraph {
        &self.prepared
    }
    pub fn facts(&self) -> &ReceivedFacts {
        &self.facts
    }
    pub fn bundle_artifact(&self) -> &Artifact {
        &self.bundle
    }
    pub fn git_objects(&self) -> &BTreeSet<ObjectId> {
        &self.git_objects
    }
    pub fn records(&self) -> &[crate::RecordSelection] {
        &self.prepared.bundle.records
    }
    pub fn roots(&self) -> &[alt_remote_protocol::RecordRoot] {
        &self.prepared.bundle.roots
    }
}
