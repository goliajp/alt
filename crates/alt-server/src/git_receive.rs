use crate::Output;
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_pack::{BaseReader, BaseSource, IngressLimits, IngressObject, PackError, ingest_pack};
use alt_remote_protocol::{
    Chunk, ContentEntry, ContentKind, Manifest, ObjectFormat, Publication, RefUpdate, RequestKey,
    VERSION,
};
use alt_repository_service::{Node, ObjectReader, ObjectSource, verify_graph};
use alt_server_store::{Credential, Error, ReadSnapshot, Result, Store};
use rand_core::{OsRng, RngCore};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};

struct Existing<'a> {
    store: &'a mut Store,
    credential: &'a Credential,
    snapshot: &'a ReadSnapshot,
    allowed: BTreeSet<ObjectId>,
}
impl BaseSource for Existing<'_> {
    fn open(&mut self, oid: ObjectId) -> std::result::Result<Option<BaseReader<'_>>, PackError> {
        if !self.allowed.contains(&oid) {
            return Ok(None);
        }
        let (kind, bytes, file) = self
            .store
            .open_snapshot_object(self.credential, self.snapshot, oid)
            .map_err(|_| PackError::Format("authorized thin-pack base unavailable"))?;
        Ok(Some(BaseReader {
            kind,
            bytes,
            reader: Box::new(file),
        }))
    }
}
struct Overlay<'a> {
    existing: RefCell<Existing<'a>>,
    incoming: &'a BTreeMap<ObjectId, IngressObject>,
}
fn ancestor(
    graph: &alt_repository_service::VerifiedGraph,
    old: ObjectId,
    new: ObjectId,
    remaining: &mut usize,
) -> Result<bool> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![new];
    while let Some(oid) = pending.pop() {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("Git fast-forward walk"))?;
        if oid == old {
            return Ok(true);
        }
        if !seen.insert(oid) {
            continue;
        }
        if let Node::Commit { parents, .. } = graph.node(oid)? {
            pending.extend(parents);
        }
    }
    Ok(false)
}
impl ObjectSource for Overlay<'_> {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        if let Some(object) = self.incoming.get(&oid) {
            return Ok(Some(ObjectReader {
                kind: object.kind,
                bytes: object.data.len() as u64,
                reader: Box::new(Cursor::new(object.data.as_slice())),
            }));
        }
        let mut existing = self.existing.borrow_mut();
        if !existing.allowed.contains(&oid) {
            return Ok(None);
        }
        let Existing {
            store,
            credential,
            snapshot,
            ..
        } = &mut *existing;
        let (kind, bytes, file) = store
            .open_snapshot_object(credential, snapshot, oid)
            .map_err(|_| {
                alt_repository_service::Error::Integrity("authorized Git base unavailable")
            })?;
        Ok(Some(ObjectReader {
            kind,
            bytes,
            reader: Box::new(file),
        }))
    }
}
pub(crate) fn receive(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    source: &mut dyn Read,
) -> Result<Output> {
    store.check_git_receive_access(credential, repository)?;
    let limits = store.limits();
    let head = alt_wire::push::parse_push_request_head_bounded(
        &mut (&mut *source).take(limits.request_bytes as u64),
        HashAlgo::Sha1,
        limits.ref_updates,
        8192,
    )
    .map_err(|_| Error::Integrity("Git receive commands"))?;
    if head.updates.is_empty() {
        return Err(Error::Integrity("empty Git push"));
    }
    for cap in &head.capabilities {
        if !matches!(
            cap.as_str(),
            "report-status" | "atomic" | "delete-refs" | "ofs-delta" | "object-format=sha1"
        ) && !cap.starts_with("agent=")
        {
            return Err(Error::Precondition("unsupported Git push capability"));
        }
    }
    let mut names = BTreeSet::new();
    for update in &head.updates {
        alt_remote_protocol::validate_ref_name(&update.name)?;
        if !names.insert(&update.name) || (update.old.is_none() && update.new.is_none()) {
            return Err(Error::Integrity("invalid or duplicate Git update"));
        }
    }
    let snapshot = store.read_snapshot(credential, repository, 300_000, limits.graph_objects)?;
    let result = receive_selected(store, credential, &snapshot, head, source);
    // A later cleanup/revocation failure must not replace a known committed
    // receipt with an apparent push failure. Temporary pins expire normally.
    let _ = store.release_snapshot(credential, snapshot);
    result
}
fn receive_selected(
    store: &mut Store,
    credential: &Credential,
    snapshot: &ReadSnapshot,
    head: alt_wire::push::PushHead,
    source: &mut dyn Read,
) -> Result<Output> {
    let limits = store.limits();
    let mut prefix = [0u8; 1];
    let count = source.read(&mut prefix)?;
    let mut existing = Existing {
        store,
        credential,
        snapshot,
        allowed: snapshot.objects().map(|(oid, _, _)| oid).collect(),
    };
    let mut objects = BTreeMap::new();
    if count != 0 {
        let mut input = Cursor::new(&prefix[..count]).chain(source);
        let pack = ingest_pack(
            &mut input,
            HashAlgo::Sha1,
            IngressLimits {
                wire_bytes: limits.total_bytes,
                objects: limits.graph_objects,
                object_bytes: limits.object_bytes,
                inflated_bytes: limits.total_bytes,
                resolved_bytes: limits.total_bytes,
                delta_depth: 64,
                steps: limits.walk_steps,
            },
            &mut existing,
        )
        .map_err(crate::git::pack_error)?;
        for object in pack.objects {
            objects.insert(object.oid, object);
        }
    }
    let roots: Vec<_> = head
        .updates
        .iter()
        .flat_map(|update| [update.old, update.new])
        .flatten()
        .collect();
    let overlay = Overlay {
        existing: RefCell::new(existing),
        incoming: &objects,
    };
    let graph = verify_graph(&overlay, HashAlgo::Sha1, &roots, limits.into())?;
    let mut refs = Vec::new();
    let mut remaining = limits.walk_steps;
    for update in &head.updates {
        let force = match (update.old, update.new) {
            (Some(old), Some(new)) => {
                update.name.starts_with("refs/tags/")
                    || !matches!(graph.node(old)?, Node::Commit { .. })
                    || !matches!(graph.node(new)?, Node::Commit { .. })
                    || !ancestor(&graph, old, new, &mut remaining)?
            }
            _ => false,
        };
        refs.push(RefUpdate {
            name: update.name.clone(),
            expected: update.old.map(|oid| oid.to_string()),
            new: update.new.map(|oid| oid.to_string()),
            force,
        });
    }
    let Existing { store, .. } = overlay.existing.into_inner();
    let principal = store.principal(credential)?;
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let request_id = format!(
        "git-{}",
        nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let key = RequestKey {
        scope: snapshot.scope.clone(),
        actor_id: principal.actor_id,
        request_id,
    };
    let mut entries = Vec::new();
    for object in objects.values() {
        let kind = match object.kind {
            ObjectKind::Blob => ContentKind::Blob,
            ObjectKind::Tree => ContentKind::Tree,
            ObjectKind::Commit => ContentKind::Commit,
            ObjectKind::Tag => ContentKind::Tag,
        };
        let mut chunks = Vec::new();
        let mut offset = 0u64;
        for chunk in object
            .data
            .chunks(usize::try_from(limits.chunk_bytes).map_err(|_| Error::Limit("chunk size"))?)
        {
            chunks.push(Chunk {
                offset,
                bytes: chunk.len() as u64,
                blake3: blake3::hash(chunk).to_hex().to_string(),
            });
            offset += chunk.len() as u64;
        }
        entries.push(ContentEntry {
            kind,
            blake3: blake3::hash(&object.data).to_hex().to_string(),
            bytes: object.data.len() as u64,
            git_oid: Some(object.oid.to_string()),
            chunks,
        });
    }
    let manifest = Manifest {
        protocol_version: VERSION,
        object_format: ObjectFormat::Sha1,
        entries,
    };
    let publication = Publication {
        protocol_version: VERSION,
        key: key.clone(),
        attribution: principal.attribution,
        object_format: ObjectFormat::Sha1,
        refs,
        record_roots: vec![],
        content_manifest: manifest.id(limits)?,
    };
    // Git has no native durable client request ID. This generated ID records
    // the server transaction and is returned for inspection, not auto-replay.
    store.begin(credential, &publication, &manifest, 60_000)?;
    for entry in &manifest.entries {
        let oid = entry
            .git_oid
            .as_ref()
            .unwrap()
            .parse()
            .map_err(|_| Error::Integrity("Git manifest oid"))?;
        let object = &objects[&oid];
        let proof = store.bytes().put(
            &key.scope,
            &entry.blake3,
            entry.bytes,
            limits.object_bytes,
            &mut Cursor::new(object.data.as_slice()),
        )?;
        store.register_content(credential, &key, entry, &proof)?;
    }
    let work = store.acquire_publication(credential, &key, 30_000, limits.graph_objects)?;
    let graph = verify_graph(work.source(), HashAlgo::Sha1, &work.roots()?, limits.into())?;
    store.mark_ready(credential, &work, &graph)?;
    let result = store.publish(credential, &work, &graph);
    let message = match &result {
        Ok(_) => None,
        Err(Error::OutcomeUnknown) => {
            Some("outcome unknown; inspect remote refs and the server request")
        }
        Err(Error::Unauthorized) => Some("access revoked"),
        Err(_) => Some("publication rejected; inspect current refs and policy"),
    };
    let commands: Vec<_> = head
        .updates
        .iter()
        .map(|update| match message {
            Some(message) => alt_wire::push::CommandStatus::Ng {
                name: update.name.clone(),
                reason: message.into(),
            },
            None => alt_wire::push::CommandStatus::Ok(update.name.clone()),
        })
        .collect();
    if result.is_err() {
        let _ = store.release_publication(credential, &work);
    }
    let mut body = Vec::new();
    if head.capabilities.iter().any(|cap| cap == "report-status") {
        alt_wire::push::encode_report_status(&mut body, Ok(()), &commands)?;
    } else if result.is_err() {
        result?;
    }
    Ok(Output::GitReport {
        bytes: body,
        request: key,
    })
}
