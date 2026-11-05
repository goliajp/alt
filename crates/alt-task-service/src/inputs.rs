use crate::{Error, Result, TaskLimits};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_repository_service::{
    GraphLimits, Node, ObjectReader, ObjectSource, VerifiedGraph, verify_object,
};
use std::{collections::BTreeMap, io::Read};

pub(crate) fn oid(value: &str) -> Result<ObjectId> {
    let oid: ObjectId = value
        .parse()
        .map_err(|_| Error::Integrity("task Git object id"))?;
    if oid.algo() != alt_git_codec::HashAlgo::Sha1 {
        return Err(Error::Integrity("task Git object format"));
    }
    Ok(oid)
}
pub(crate) fn tree_files(
    graph: &VerifiedGraph,
    tree: ObjectId,
    limits: TaskLimits,
    remaining: &mut usize,
) -> Result<BTreeMap<String, (u32, ObjectId)>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![(String::new(), tree)];
    while let Some((prefix, tree)) = pending.pop() {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("task tree walk"))?;
        let Node::Tree(entries) = graph.node(tree)? else {
            return Err(Error::Integrity("candidate tree type"));
        };
        for entry in entries {
            *remaining = remaining
                .checked_sub(1)
                .ok_or(Error::Limit("task tree walk"))?;
            let name = std::str::from_utf8(&entry.name)
                .map_err(|_| Error::Integrity("task input requires lossless UTF-8 paths"))?;
            let path = format!("{prefix}{name}");
            if path.len() > limits.path_bytes {
                return Err(Error::Limit("task input path bytes"));
            }
            alt_input::checked_path(path.as_bytes())?;
            if entry.mode == 0o040000 {
                pending.push((format!("{path}/"), entry.oid));
            } else {
                if !matches!(entry.mode, 0o100644 | 0o100755 | 0o120000) {
                    return Err(Error::Integrity("unsupported task input mode"));
                }
                if files.insert(path, (entry.mode, entry.oid)).is_some() {
                    return Err(Error::Integrity("duplicate input tree path"));
                }
            }
            if files.len().saturating_add(pending.len()) > limits.paths {
                return Err(Error::Limit("task input paths"));
            }
        }
    }
    Ok(files)
}
struct DigestReader<'a> {
    reader: Box<dyn Read + 'a>,
    hash: &'a mut blake3::Hasher,
}
impl Read for DigestReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let n = self.reader.read(buffer)?;
        self.hash.update(&buffer[..n]);
        Ok(n)
    }
}
pub fn verify_candidate_inputs(
    candidate: &alt_task::Candidate,
    task: &alt_task::Task,
    manifest: &alt_input::Manifest,
    graph: &VerifiedGraph,
    source: &impl ObjectSource,
    limits: TaskLimits,
) -> Result<()> {
    verify_candidate_inputs_bounded(candidate, task, manifest, graph, source, limits, &mut {
        limits.walk_steps
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_candidate_inputs_bounded(
    candidate: &alt_task::Candidate,
    task: &alt_task::Task,
    manifest: &alt_input::Manifest,
    graph: &VerifiedGraph,
    source: &impl ObjectSource,
    limits: TaskLimits,
    remaining: &mut usize,
) -> Result<()> {
    use alt_task::Record;
    candidate.validate()?;
    task.validate()?;
    manifest.validate()?;
    let mut auxiliary = task.spec.auxiliary.clone();
    auxiliary.sort_by(|a, b| a.path.cmp(&b.path));
    let mut exclusions = task.spec.exclusions.clone();
    exclusions.sort();
    exclusions.dedup();
    if candidate.task != task.id()?
        || candidate.manifest != manifest.id()?
        || candidate.target_expected != task.target_expected
        || manifest.context != task.spec.validation
        || manifest.plan.auxiliary != auxiliary
        || manifest.plan.exclusions != exclusions
    {
        return Err(Error::Integrity("candidate task/input binding"));
    }
    let files = tree_files(graph, oid(&candidate.tree)?, limits, remaining)?;
    let source_entries: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .filter(|e| e.role == alt_input::InputRole::Source)
        .map(|e| (&e.path, &e.node))
        .collect();
    if files.len() != source_entries.len() {
        return Err(Error::Integrity("candidate source file set"));
    }
    let mut total = 0u64;
    for (path, (mode, id)) in &files {
        let node = source_entries
            .get(path)
            .ok_or(Error::Integrity("candidate source file set"))?;
        let expected_kind = if *mode == 0o120000 {
            alt_input::EntryKind::Symlink
        } else {
            alt_input::EntryKind::File
        };
        if node.kind != expected_kind || node.executable != (*mode == 0o100755) {
            return Err(Error::Integrity("candidate source mode"));
        }
        let object = source
            .open(*id)?
            .ok_or(Error::Integrity("candidate source bytes missing"))?;
        if object.kind != ObjectKind::Blob || object.bytes != node.len {
            return Err(Error::Integrity("candidate source byte size"));
        }
        total = total
            .checked_add(object.bytes)
            .ok_or(Error::Limit("candidate source bytes"))?;
        if total > limits.total_bytes {
            return Err(Error::Limit("candidate source bytes"));
        }
        let mut hash = blake3::Hasher::new();
        verify_object(
            *id,
            ObjectReader {
                kind: object.kind,
                bytes: object.bytes,
                reader: Box::new(DigestReader {
                    reader: object.reader,
                    hash: &mut hash,
                }),
            },
            GraphLimits {
                objects: limits.objects,
                edges: limits.edges,
                object_bytes: limits.artifact_bytes,
                total_bytes: limits.total_bytes,
                structured_bytes: limits.structured_bytes,
                paths: limits.paths,
                path_bytes: limits.path_bytes,
                walk_steps: limits.walk_steps,
            },
        )?;
        if hash.finalize().to_hex().as_str() != node.content {
            return Err(Error::Integrity("candidate source manifest digest"));
        }
    }
    let Node::Commit { tree, .. } = graph.node(oid(&task.baseline.head)?)? else {
        return Err(Error::Integrity("task baseline head type"));
    };
    if *tree != oid(&task.baseline.tree)? {
        return Err(Error::Integrity("task baseline tree"));
    }
    let before = tree_files(graph, *tree, limits, remaining)?;
    for path in before.keys().chain(files.keys()) {
        if before.get(path) != files.get(path) && !task.spec.allows(path) {
            return Err(Error::Integrity("candidate changes outside task scope"));
        }
    }
    Ok(())
}

pub(crate) fn object_identity(
    source: &impl ObjectSource,
    id: ObjectId,
    limits: TaskLimits,
) -> Result<alt_delivery::ObjectIdentity> {
    let object = source
        .open(id)?
        .ok_or(Error::Integrity("selected Git object missing"))?;
    let kind = object.kind.to_string();
    let bytes = object.bytes;
    let mut hash = blake3::Hasher::new();
    verify_object(
        id,
        ObjectReader {
            kind: object.kind,
            bytes,
            reader: Box::new(DigestReader {
                reader: object.reader,
                hash: &mut hash,
            }),
        },
        GraphLimits {
            objects: limits.objects,
            edges: limits.edges,
            object_bytes: limits.artifact_bytes,
            total_bytes: limits.total_bytes,
            structured_bytes: limits.structured_bytes,
            paths: limits.paths,
            path_bytes: limits.path_bytes,
            walk_steps: limits.walk_steps,
        },
    )?;
    Ok(alt_delivery::ObjectIdentity {
        kind,
        bytes,
        blake3: hash.finalize().to_hex().to_string(),
    })
}
