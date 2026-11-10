use crate::{
    ArtifactSource, Error, PreparedTaskGraph, Result, TaskBundle, TaskLimits, VerifiedTaskGraph,
};
use alt_remote_protocol::{ContentKind, Limits, Manifest, Publication};
use alt_repository_service::{ObjectSource, VerifiedGraph, verify_graph};
use std::{cell::Cell, io::Read};

/// Immutable byte validation shared by the client preview and server worker.
/// This is never a permission grant or a current approval.
pub struct VerifiedPublication {
    pub graph: VerifiedGraph,
    pub tasks: Option<VerifiedTaskGraph>,
}
struct SelectionSource<'a, A> {
    inner: &'a A,
    bytes: Cell<u64>,
    reads: Cell<usize>,
}
impl<A: ArtifactSource> ArtifactSource for SelectionSource<'_, A> {
    fn open(&self, artifact: &alt_task::Artifact) -> crate::Result<Box<dyn Read + '_>> {
        self.bytes.set(
            self.bytes
                .get()
                .checked_sub(artifact.bytes)
                .ok_or(crate::Error::Limit("task root discovery bytes"))?,
        );
        self.reads.set(
            self.reads
                .get()
                .checked_sub(1)
                .ok_or(crate::Error::Limit("task root discovery reads"))?,
        );
        self.inner.open(artifact)
    }
}
impl VerifiedPublication {
    pub fn verify(
        publication: &Publication,
        manifest: &Manifest,
        objects: &impl ObjectSource,
        artifacts: &impl ArtifactSource,
        limits: Limits,
        task_limits: TaskLimits,
    ) -> Result<Self> {
        manifest
            .validate_publication(publication, limits)
            .map_err(|_| Error::Integrity("publication manifest"))?;
        let mut roots = publication
            .refs
            .iter()
            .flat_map(|update| [&update.expected, &update.new])
            .flatten()
            .map(|oid| {
                oid.parse()
                    .map_err(|_| Error::Integrity("publication object id"))
            })
            .collect::<Result<Vec<alt_git_codec::ObjectId>>>()?;
        let has_tasks = !publication.record_roots.is_empty();
        if has_tasks {
            let selected = manifest
                .entries
                .iter()
                .find(|entry| entry.kind == ContentKind::TaskBundle)
                .ok_or(Error::Integrity("missing task bundle"))?;
            if selected.bytes > task_limits.structured_bytes as u64 {
                return Err(Error::Limit("task bundle bytes"));
            }
            let source = SelectionSource {
                inner: artifacts,
                bytes: Cell::new(task_limits.work_bytes),
                reads: Cell::new(task_limits.walk_steps),
            };
            let mut raw = Vec::new();
            source
                .open(&alt_task::Artifact {
                    blake3: selected.blake3.clone(),
                    bytes: selected.bytes,
                })?
                .take(selected.bytes.saturating_add(1))
                .read_to_end(&mut raw)?;
            if raw.len() as u64 != selected.bytes
                || alt_hash::blake3::hash(&raw).to_hex().as_str() != selected.blake3
            {
                return Err(Error::Integrity("task bundle bytes"));
            }
            let bundle = TaskBundle::decode(&raw, task_limits)?;
            let prepared =
                PreparedTaskGraph::load(bundle, &publication.record_roots, &source, task_limits)?;
            roots.extend(prepared.object_roots(&source)?);
        }
        roots.sort_unstable();
        roots.dedup();
        let algo = match publication.object_format {
            alt_remote_protocol::ObjectFormat::Sha1 => alt_git_codec::HashAlgo::Sha1,
            alt_remote_protocol::ObjectFormat::Sha256 => alt_git_codec::HashAlgo::Sha256,
        };
        let graph = verify_graph(objects, algo, &roots, limits.into())?;
        let tasks = if has_tasks {
            Some(VerifiedTaskGraph::verify(
                publication,
                manifest,
                limits,
                &graph,
                objects,
                artifacts,
                task_limits,
            )?)
        } else {
            None
        };
        Ok(Self { graph, tasks })
    }
}
