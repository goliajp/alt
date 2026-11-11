//! Git smart HTTP over verified pack segments: a push is verified once and stored as one
//! immutable segment published with its refs; reads resend stored bytes.

use crate::{Output, git::Command};
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_pack_segment::{Kind, Limits, Oid, Segment, SegmentSet, ingest};
use alt_remote_protocol::{Manifest, ObjectFormat, Publication, RefUpdate, RequestKey, VERSION};
use alt_repository_service::{Graph, GraphLimits, Node, parse_node};
use alt_server_store::{Credential, Error, NewSegment, PackView, Result, Store};
use alt_wire::pkt;
use std::{
    borrow::Cow,
    collections::{BTreeSet, HashSet},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    sync::Arc,
};

const UPLOAD_RESULT: &str = "application/x-git-upload-pack-result";

fn raw(oid: ObjectId) -> Oid {
    oid.as_bytes().try_into().expect("sha1 object id")
}

fn id(oid: &Oid) -> ObjectId {
    ObjectId::from_bytes(HashAlgo::Sha1, oid).expect("sha1 object id")
}

fn segment_error(error: alt_pack_segment::Error) -> Error {
    use alt_pack_segment::Error as E;
    match error {
        E::Io(error) => Error::Io(error),
        E::Limit(reason) => Error::Limit(reason),
        E::Format(reason) | E::Corrupt(reason) => Error::Integrity(reason),
        E::Dangling(..) | E::MissingBase(_) | E::NotFound(_) => {
            Error::Integrity("pack references an unavailable object")
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// loads commits from the stored commit graph and other objects on demand
struct PackGraph<'a> {
    set: &'a SegmentSet,
    limits: GraphLimits,
}

impl Graph for PackGraph<'_> {
    fn node(&self, oid: ObjectId) -> alt_repository_service::Result<Cow<'_, Node>> {
        use alt_repository_service::Error as E;
        let key = raw(oid);
        if let Some(commit) = self.set.commit(&key) {
            return Ok(Cow::Owned(Node::Commit {
                tree: id(&commit.tree),
                parents: commit.parents.iter().map(id).collect(),
            }));
        }
        let kind = match self.set.kind(&key) {
            Ok(Some(Kind::Blob)) => return Ok(Cow::Owned(Node::Blob)),
            Ok(Some(Kind::Tree)) => ObjectKind::Tree,
            Ok(Some(Kind::Tag)) => ObjectKind::Tag,
            Ok(Some(Kind::Commit)) => {
                return Err(E::Integrity("commit missing from the commit graph"));
            }
            Ok(None) => return Err(E::Missing(oid)),
            Err(_) => return Err(E::Integrity("pack segment record")),
        };
        let (_, data) = self
            .set
            .read(&key)
            .map_err(|_| E::Integrity("pack segment object"))?;
        parse_node(kind, &data, HashAlgo::Sha1).map(Cow::Owned)
    }

    fn graph_limits(&self) -> GraphLimits {
        self.limits
    }
}

/// whether `old` is an ancestor of `new`; generations prune everything older than `old`
fn ancestor(set: &SegmentSet, old: &Oid, new: &Oid, remaining: &mut usize) -> Result<bool> {
    let Some(floor) = set.commit(old).map(|commit| commit.generation) else {
        return Ok(false);
    };
    let mut seen = HashSet::new();
    let mut pending = vec![*new];
    while let Some(oid) = pending.pop() {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("Git fast-forward walk"))?;
        if oid == *old {
            return Ok(true);
        }
        if !seen.insert(oid) {
            continue;
        }
        if let Some(commit) = set.commit(&oid)
            && commit.generation > floor
        {
            pending.extend(commit.parents);
        }
    }
    Ok(false)
}

pub(crate) fn receive(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    source: &mut dyn Read,
    view: PackView,
) -> Result<Output> {
    let limits = store.limits();
    // Before a body too large for its buffer, git sends a lone flush packet to probe
    // authentication; it expects an empty success and then sends the real request.
    let mut probe = [0u8; 4];
    let mut filled = 0;
    while filled < probe.len() {
        match source.read(&mut probe[filled..])? {
            0 => break,
            count => filled += count,
        }
    }
    let mut rest = [0u8; 1];
    let more = if filled == probe.len() {
        source.read(&mut rest)?
    } else {
        0
    };
    if &probe[..filled] == b"0000" && more == 0 {
        return Ok(Output::Bytes {
            content_type: "application/x-git-receive-pack-result",
            bytes: Vec::new(),
        });
    }
    let mut replay = std::io::Cursor::new([&probe[..filled], &rest[..more]].concat()).chain(source);
    let source: &mut dyn Read = &mut replay;
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
    // as in Git, capabilities the server does not know are ignored, not refused
    let mut names = BTreeSet::new();
    for update in &head.updates {
        alt_remote_protocol::validate_ref_name(&update.name)?;
        if !names.insert(&update.name) || (update.old.is_none() && update.new.is_none()) {
            return Err(Error::Integrity("invalid or duplicate Git update"));
        }
    }
    // A pack that fits with its row is handled in memory. A larger one goes straight
    // into its segment file and is read back through a mapping, so its size is never
    // held on the heap.
    let mut pack = Vec::new();
    let inline = alt_server_store::INLINE_SEGMENT_BYTES;
    (&mut *source)
        .take(inline as u64 + 1)
        .read_to_end(&mut pack)?;
    let mut spooled = None;
    if pack.len() > inline {
        spooled = Some(spool(store, repository, &pack, source, limits.total_bytes)?);
        pack = Vec::new();
    }
    let mut created = spooled.as_ref().map(|spooled| spooled.path.clone());
    let mut published = false;
    let result = match &mut spooled {
        Some(Spooled {
            id,
            path,
            file,
            pack,
        }) => publish(
            store,
            credential,
            repository,
            &view,
            &head,
            pack,
            Some((id, path, file)),
            &mut created,
            &mut published,
        ),
        None => publish(
            store,
            credential,
            repository,
            &view,
            &head,
            &pack,
            None,
            &mut created,
            &mut published,
        ),
    };
    if !published && let Some(path) = &created {
        // nothing references an unpublished segment
        let _ = std::fs::remove_file(path);
    }
    let (request, outcome) = match result {
        Ok(request) => (request, Ok(())),
        Err(error) => (None, Err(error)),
    };
    let message = match &outcome {
        Ok(()) => None,
        Err(Error::OutcomeUnknown) => {
            Some("outcome unknown; inspect remote refs and the server request".to_owned())
        }
        Err(Error::Unauthorized) => Some("access revoked".to_owned()),
        // the operator can raise a limit; the pusher has to know which one it was
        Err(Error::Limit(what)) => Some(format!("server limit exceeded: {what}")),
        Err(_) => Some("publication rejected; inspect current refs and policy".to_owned()),
    };
    if !head.capabilities.iter().any(|cap| cap == "report-status") {
        outcome?;
        return Ok(Output::Bytes {
            content_type: "application/x-git-receive-pack-result",
            bytes: Vec::new(),
        });
    }
    let commands: Vec<_> = head
        .updates
        .iter()
        .map(|update| match &message {
            Some(message) => alt_wire::push::CommandStatus::Ng {
                name: update.name.clone(),
                reason: message.clone(),
            },
            None => alt_wire::push::CommandStatus::Ok(update.name.clone()),
        })
        .collect();
    let mut report = Vec::new();
    alt_wire::push::encode_report_status(&mut report, Ok(()), &commands)?;
    let body = if head.capabilities.iter().any(|cap| cap == "side-band-64k") {
        // the report travels on band 1, closed by a flush packet
        let mut framed = Vec::with_capacity(report.len() + 16);
        for chunk in report.chunks(65515) {
            framed.extend_from_slice(format!("{:04x}", chunk.len() + 5).as_bytes());
            framed.push(1);
            framed.extend_from_slice(chunk);
        }
        pkt::write_flush(&mut framed)?;
        framed
    } else {
        report
    };
    match request {
        Some(request) => Ok(Output::GitReport {
            bytes: body,
            request,
        }),
        None => Ok(Output::Bytes {
            content_type: "application/x-git-receive-pack-result",
            bytes: body,
        }),
    }
}

/// a pack that was written to its segment file as it arrived
struct Spooled {
    id: String,
    path: std::path::PathBuf,
    file: std::fs::File,
    pack: alt_os::map::Mmap,
}

/// Writes `first` and the rest of `source` to a new segment file and maps it.
fn spool(
    store: &Store,
    repository: &str,
    first: &[u8],
    source: &mut dyn Read,
    limit: u64,
) -> Result<Spooled> {
    let mut nonce = [0u8; 16];
    alt_os::random::fill(&mut nonce);
    let id = hex(&nonce);
    let path = store.pack_path(repository, &id)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let mut written = || -> Result<Spooled> {
        file.write_all(first)?;
        let rest = limit.saturating_add(1).saturating_sub(first.len() as u64);
        let copied = std::io::copy(&mut source.take(rest), &mut file)?;
        if first.len() as u64 + copied > limit {
            return Err(Error::Limit("Git pack bytes"));
        }
        // the file is new and only this request writes to it; the index it gets
        // later lies behind what is mapped here
        let pack = unsafe { alt_os::map::Mmap::map(&file) }?;
        Ok(Spooled {
            id: id.clone(),
            path: path.clone(),
            file: file.try_clone()?,
            pack,
        })
    };
    let spooled = written();
    if spooled.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    spooled
}

#[expect(
    clippy::too_many_arguments,
    reason = "one caller; the parts of one request"
)]
fn publish(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    view: &PackView,
    head: &alt_wire::push::PushHead,
    pack: &[u8],
    spooled: Option<(&String, &std::path::PathBuf, &mut std::fs::File)>,
    created: &mut Option<std::path::PathBuf>,
    published: &mut bool,
) -> Result<Option<RequestKey>> {
    let limits = store.limits();
    // A client may list refs it does not change. They take no part in the publication,
    // but a stale expectation is still a conflict.
    let mut changes = Vec::new();
    for update in &head.updates {
        if update.old != update.new {
            changes.push(update);
        } else if view.refs.get(&update.name).copied() != update.old {
            return Err(Error::Precondition("reference changed since it was read"));
        }
    }
    if changes.is_empty() {
        return Ok(None);
    }
    let mut nonce = [0u8; 40];
    alt_os::random::fill(&mut nonce);
    let mut segments = view.segments.segments().to_vec();
    let mut stored = None;
    if !pack.is_empty() {
        let ingested =
            alt_server_store::telemetry::observe(alt_server_telemetry::Phase::Decode, || {
                ingest(
                    pack,
                    &view.segments,
                    &Limits {
                        objects: limits.graph_objects as u64,
                        object_bytes: limits.object_bytes,
                        inflated_bytes: limits.total_bytes,
                        delta_depth: 4095,
                    },
                )
                .map_err(segment_error)
            })?;
        if ingested.objects() > 0 {
            let (segment_id, path, bytes, inline) = match spooled {
                Some((id, path, file)) => {
                    // the pack is in its file already; the index goes behind it
                    let mut index = Vec::with_capacity(ingested.objects() * 96 + 2048);
                    ingested.write_index(&mut index).map_err(segment_error)?;
                    file.write_all(&index)?;
                    file.sync_all()?;
                    std::fs::File::open(path.parent().expect("segment directory"))?.sync_all()?;
                    let bytes = (pack.len() + index.len()) as u64;
                    (id.clone(), path.clone(), bytes, None)
                }
                None => {
                    let segment_id = hex(&nonce[..16]);
                    let path = store.pack_path(repository, &segment_id)?;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&path)?;
                    *created = Some(path.clone());
                    let mut content =
                        Vec::with_capacity(pack.len() + ingested.objects() * 96 + 2048);
                    content.extend_from_slice(pack);
                    ingested.write_index(&mut content).map_err(segment_error)?;
                    file.write_all(&content)?;
                    let bytes = content.len() as u64;
                    let inline = content.len() <= alt_server_store::INLINE_SEGMENT_BYTES;
                    if !inline {
                        file.sync_all()?;
                        std::fs::File::open(path.parent().expect("segment directory"))?
                            .sync_all()?;
                    }
                    (segment_id, path, bytes, inline.then_some(content))
                }
            };
            let segment = Arc::new(Segment::open(&path).map_err(segment_error)?);
            segments.push(segment.clone());
            stored = Some((
                segment_id,
                ingested.objects() as u64,
                ingested.flags(),
                segment,
                inline,
                bytes,
            ));
        }
    }
    let set = SegmentSet::new(segments);
    let mut remaining = limits.walk_steps;
    let mut refs = Vec::new();
    for update in changes {
        let force = match (update.old, update.new) {
            (Some(old), Some(new)) => {
                update.name.starts_with("refs/tags/")
                    || !ancestor(&set, &raw(old), &raw(new), &mut remaining)?
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
    let key = RequestKey {
        scope: view.scope.clone(),
        actor_id: view.actor_id.clone(),
        request_id: format!("git-{}", hex(&nonce[16..])),
    };
    let manifest = Manifest {
        protocol_version: VERSION,
        object_format: ObjectFormat::Sha1,
        entries: vec![],
    };
    let publication = Publication {
        protocol_version: VERSION,
        key: key.clone(),
        attribution: view.attribution.clone(),
        object_format: ObjectFormat::Sha1,
        refs,
        record_roots: vec![],
        content_manifest: manifest.id(limits)?,
    };
    let graph = PackGraph {
        set: &set,
        limits: limits.into(),
    };
    let segment = stored
        .as_ref()
        .map(|(id, objects, flags, _, inline, bytes)| NewSegment {
            id,
            objects: *objects,
            bytes: *bytes,
            flags: *flags,
            inline: inline.as_deref(),
        });
    // Git has no durable client request id; the generated one records the server
    // transaction for inspection, not for replay
    alt_server_store::telemetry::observe(alt_server_telemetry::Phase::Commit, || {
        store.pack_publish(credential, &publication, &graph, segment)
    })?;
    *published = stored.is_some();
    if let Some((id, _, _, segment, _, _)) = stored {
        store.adopt_segment(repository, &id, segment)?;
    }
    Ok(Some(key))
}

fn peel(set: &SegmentSet, mut oid: Oid, remaining: &mut usize) -> Result<Option<Oid>> {
    let mut peeled = None;
    while set.kind(&oid).map_err(segment_error)? == Some(Kind::Tag) {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("tag peel walk"))?;
        let (_, data) = set.read(&oid).map_err(segment_error)?;
        let Node::Tag { target, .. } = parse_node(ObjectKind::Tag, &data, HashAlgo::Sha1)? else {
            return Err(Error::Integrity("tag object"));
        };
        oid = raw(target);
        peeled = Some(oid);
    }
    Ok(peeled)
}

pub(crate) fn read(
    limits: alt_remote_protocol::Limits,
    receive: bool,
    command: &Command,
    view: PackView,
) -> Result<Output> {
    let bytes = |content_type, bytes| {
        Ok(Output::Bytes {
            content_type,
            bytes,
        })
    };
    let mut output = Vec::new();
    if matches!(command, Command::Advertisement) {
        if receive {
            let refs: Vec<_> = view
                .refs
                .iter()
                .map(|(name, oid)| (name.clone(), *oid))
                .collect();
            alt_wire::push::encode_v1_ref_advertisement(
                &mut output,
                &refs,
                &[
                    "report-status",
                    "atomic",
                    "delete-refs",
                    "ofs-delta",
                    "side-band-64k",
                    "object-format=sha1",
                    "agent=alt-server",
                ],
                HashAlgo::Sha1,
            )?;
            return bytes("application/x-git-receive-pack-advertisement", output);
        }
        pkt::write_data(&mut output, b"# service=git-upload-pack\n")?;
        pkt::write_flush(&mut output)?;
        for line in [
            "version 2\n",
            "agent=alt-server\n",
            "ls-refs=unborn\n",
            "fetch=wait-for-done\n",
            "object-format=sha1\n",
        ] {
            pkt::write_data(&mut output, line.as_bytes())?;
        }
        pkt::write_flush(&mut output)?;
        return bytes("application/x-git-upload-pack-advertisement", output);
    }
    let mut remaining = limits.walk_steps;
    if let Command::Refs(request, unborn) = command {
        let wanted = |name: &str| {
            request.ref_prefixes.is_empty()
                || request
                    .ref_prefixes
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        };
        let mut refs = view.refs.clone();
        if let Some(oid) = refs.get(&view.default_branch).copied() {
            refs.insert("HEAD".into(), oid);
        }
        for (name, oid) in refs {
            if !wanted(&name) {
                continue;
            }
            let mut line = format!("{oid} {name}");
            if request.symrefs && name == "HEAD" {
                line.push_str(&format!(" symref-target:{}", view.default_branch));
            }
            if request.peel
                && let Some(target) = peel(&view.segments, raw(oid), &mut remaining)?
            {
                line.push_str(&format!(" peeled:{}", id(&target)));
            }
            line.push('\n');
            pkt::write_data(&mut output, line.as_bytes())?;
        }
        if !view.refs.contains_key(&view.default_branch) && *unborn && wanted("HEAD") {
            pkt::write_data(
                &mut output,
                format!("unborn HEAD symref-target:{}\n", view.default_branch).as_bytes(),
            )?;
        }
        pkt::write_flush(&mut output)?;
        return bytes(UPLOAD_RESULT, output);
    }
    let Command::Fetch(request) = command else {
        return Err(Error::Integrity("Git command dispatch"));
    };
    if request.wants.is_empty() {
        return Err(Error::Integrity("empty Git wants"));
    }
    // as in Git's default configuration, only current ref tips can be asked for
    let tips: HashSet<Oid> = view.refs.values().map(|oid| raw(*oid)).collect();
    let wants: Vec<Oid> = request.wants.iter().map(|oid| raw(*oid)).collect();
    if wants.iter().any(|oid| !tips.contains(oid)) {
        return Err(Error::Unauthorized);
    }
    let set = &view.segments;
    let haves: Vec<Oid> = request
        .haves
        .iter()
        .map(|oid| raw(*oid))
        .filter(|oid| set.commit(oid).is_some())
        .collect();
    if !request.done {
        pkt::write_data(&mut output, b"acknowledgments\n")?;
        if haves.is_empty() {
            pkt::write_data(&mut output, b"NAK\n")?;
        } else {
            for oid in &haves {
                pkt::write_data(&mut output, format!("ACK {}\n", id(oid)).as_bytes())?;
            }
        }
        pkt::write_flush(&mut output)?;
        return bytes(UPLOAD_RESULT, output);
    }
    let everything = haves.is_empty()
        && !view.unreachable
        && set.verbatim()
        && tips.iter().all(|tip| wants.contains(tip));
    if everything {
        // nothing is computed per object: the stored bytes go out as they are read
        let (pieces, checksum) = set.whole().map_err(segment_error)?;
        return Ok(Output::WholePack(pieces, checksum));
    }
    let mut selection = set.select(&wants, &haves).map_err(segment_error)?;
    if request.include_tag {
        let chosen: HashSet<&Oid> = selection.objects().iter().collect();
        let mut more = wants.clone();
        for (name, oid) in &view.refs {
            let tag = raw(*oid);
            if name.starts_with("refs/tags/")
                && !chosen.contains(&tag)
                && let Some(target) = peel(set, tag, &mut remaining)?
                && chosen.contains(&target)
            {
                more.push(tag);
            }
        }
        if more.len() > wants.len() {
            selection = set.select(&more, &haves).map_err(segment_error)?;
        }
    }
    // the pack is framed as it is written; a large one is not kept in memory
    let mut body = crate::git_response::Spill::new();
    pkt::write_data(&mut body, b"packfile\n")?;
    let mut band = crate::git_response::Band::new(&mut body);
    set.write_selected(&selection, request.thin_pack, &mut band)
        .map_err(segment_error)?;
    band.finish()?;
    pkt::write_flush(&mut body)?;
    body.into_output(UPLOAD_RESULT)
}
