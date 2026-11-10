//! Git smart HTTP translation. Ref/object authorization and publication remain
//! in the same repository domain and SQL transaction as native requests.
use crate::{Output, Route};
use alt_git_codec::{HashAlgo, ObjectId};
use alt_repository_service::{Node, ObjectSource, VerifiedGraph, verify_graph, verify_object};
use alt_server_store::{Credential, Error, ReadSnapshot, Result, SnapshotObjects, Store};
use alt_wire::pkt::{self, Frame};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read, Seek, SeekFrom, Write},
};

const UPLOAD_RESULT: &str = "application/x-git-upload-pack-result";
pub(crate) fn pack_error(error: alt_git_pack::PackError) -> Error {
    match error {
        alt_git_pack::PackError::Io(error) => Error::Io(error),
        alt_git_pack::PackError::Limit(reason) => Error::Limit(reason),
        alt_git_pack::PackError::Format(reason) => Error::Integrity(reason),
    }
}
fn closure(
    graph: &VerifiedGraph,
    roots: impl IntoIterator<Item = ObjectId>,
    remaining: &mut usize,
) -> Result<BTreeSet<ObjectId>> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<_> = roots.into_iter().collect();
    while let Some(oid) = stack.pop() {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("Git graph walk"))?;
        if !seen.insert(oid) {
            continue;
        }
        match graph.node(oid)? {
            Node::Blob => {}
            Node::Commit { tree, parents } => {
                stack.push(*tree);
                stack.extend(parents);
            }
            Node::Tree(entries) => stack.extend(
                entries
                    .iter()
                    .filter(|entry| entry.mode != 0o160000)
                    .map(|entry| entry.oid),
            ),
            Node::Tag { target, .. } => stack.push(*target),
        }
    }
    Ok(seen)
}
fn peeled(graph: &VerifiedGraph, mut oid: ObjectId, remaining: &mut usize) -> Result<ObjectId> {
    loop {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("tag peel walk"))?;
        match graph.node(oid)? {
            Node::Tag { target, .. } => oid = *target,
            _ => return Ok(oid),
        }
    }
}
fn bytes(content_type: &'static str, bytes: Vec<u8>) -> Output {
    Output::Bytes {
        content_type,
        bytes,
    }
}
fn validate_command(raw: &[u8], max_lines: usize) -> Result<(String, bool)> {
    let mut input = Cursor::new(raw);
    let mut scratch = Vec::new();
    let mut command = None;
    let mut count = 0usize;
    loop {
        count += 1;
        if count > max_lines {
            return Err(Error::Limit("Git command lines"));
        }
        match pkt::read_frame(&mut input, &mut scratch)
            .map_err(|_| Error::Integrity("Git command framing"))?
        {
            Frame::Delim => break,
            Frame::Data(line) => {
                let line = std::str::from_utf8(line)
                    .map_err(|_| Error::Integrity("Git header encoding"))?
                    .trim_end_matches('\n');
                if let Some(value) = line.strip_prefix("command=") {
                    if command.is_some() || !matches!(value, "ls-refs" | "fetch") {
                        return Err(Error::Precondition("unsupported Git command"));
                    }
                    command = Some(value.to_owned());
                } else if line != "object-format=sha1" && !line.starts_with("agent=") {
                    return Err(Error::Precondition("unsupported Git command capability"));
                }
            }
            _ => return Err(Error::Integrity("Git command delimiter")),
        }
    }
    let command = command.ok_or(Error::Integrity("missing Git command"))?;
    let mut unborn = false;
    loop {
        count += 1;
        if count > max_lines {
            return Err(Error::Limit("Git command lines"));
        }
        match pkt::read_frame(&mut input, &mut scratch)
            .map_err(|_| Error::Integrity("Git argument framing"))?
        {
            Frame::Flush => break,
            Frame::Data(line) => {
                let line = std::str::from_utf8(line)
                    .map_err(|_| Error::Integrity("Git argument encoding"))?
                    .trim_end_matches('\n');
                unborn |= command == "ls-refs" && line == "unborn";
                let supported = match command.as_str() {
                    "ls-refs" => {
                        matches!(line, "peel" | "symrefs" | "unborn")
                            || line.starts_with("ref-prefix ")
                    }
                    "fetch" => {
                        matches!(
                            line,
                            "done"
                                | "no-progress"
                                | "thin-pack"
                                | "ofs-delta"
                                | "include-tag"
                                | "wait-for-done"
                        ) || line.starts_with("want ")
                            || line.starts_with("have ")
                    }
                    _ => false,
                };
                if !supported {
                    return Err(Error::Precondition(
                        "unsupported Git fetch option; shallow and filtered fetch are unavailable",
                    ));
                }
            }
            _ => return Err(Error::Integrity("Git argument terminator")),
        }
    }
    if input.position() != raw.len() as u64 {
        return Err(Error::Integrity("trailing Git command bytes"));
    }
    Ok((command, unborn))
}
enum Command {
    Advertisement,
    Refs(alt_wire::ls_refs::LsRefsRequest, bool),
    Fetch(alt_wire::fetch::FetchRequest),
}
fn command(
    source: &mut dyn Read,
    advertisement: bool,
    limits: alt_remote_protocol::Limits,
) -> Result<Command> {
    if advertisement {
        if source.read(&mut [0u8; 1])? != 0 {
            return Err(Error::Integrity("unexpected advertisement body"));
        }
        return Ok(Command::Advertisement);
    }
    let mut raw = Vec::new();
    source
        .take((limits.request_bytes as u64).saturating_add(1))
        .read_to_end(&mut raw)?;
    if raw.len() > limits.request_bytes {
        return Err(Error::Limit("Git command body"));
    }
    let (name, unborn) = validate_command(
        &raw,
        limits.graph_objects.saturating_mul(2).saturating_add(100),
    )?;
    if name == "ls-refs" {
        let (request, _) = alt_wire::ls_refs::parse_ls_refs_request(&mut Cursor::new(&raw))
            .map_err(|_| Error::Integrity("Git ls-refs request"))?;
        Ok(Command::Refs(request, unborn))
    } else {
        let (request, _) = alt_wire::fetch::parse_fetch_request(&mut Cursor::new(&raw))
            .map_err(|_| Error::Integrity("Git fetch request"))?;
        Ok(Command::Fetch(request))
    }
}

fn reference_tags(
    objects: &SnapshotObjects<'_>,
    snapshot: &ReadSnapshot,
    limits: alt_remote_protocol::Limits,
) -> Result<BTreeMap<ObjectId, Node>> {
    let metadata: BTreeMap<_, _> = snapshot
        .objects()
        .map(|(id, kind, size)| (id, (kind, size)))
        .collect();
    let mut result = BTreeMap::new();
    let mut remaining = limits.total_bytes;
    for (id, (kind, size)) in &metadata {
        if *kind != alt_git_codec::ObjectKind::Tag {
            continue;
        }
        remaining = remaining
            .checked_sub(*size)
            .ok_or(Error::Limit("reference tag bytes"))?;
        let object = objects
            .open(*id)?
            .ok_or(Error::Integrity("reference tag missing"))?;
        let node = verify_object(*id, object, limits.into())?;
        let Node::Tag { target, kind } = &node else {
            return Err(Error::Integrity("reference tag kind"));
        };
        if metadata.get(target).map(|(actual, _)| actual) != Some(kind) {
            return Err(Error::Integrity("reference tag target kind"));
        }
        result.insert(*id, node);
    }
    Ok(result)
}
fn peeled_reference(
    tags: &BTreeMap<ObjectId, Node>,
    mut id: ObjectId,
    remaining: &mut usize,
) -> Result<ObjectId> {
    while let Some(Node::Tag { target, .. }) = tags.get(&id) {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("tag peel walk"))?;
        id = *target;
    }
    Ok(id)
}

pub fn execute_git(
    store: &mut Store,
    credential: &Credential,
    route: Route,
    protocol_v2: bool,
    source: &mut dyn Read,
) -> Result<Output> {
    execute_git_with_refresh(store, credential, route, protocol_v2, source, &mut |_| {
        Ok(credential.clone())
    })
}
/// Refresh external authority after buffered read work and before its final
/// current-permission transaction. Receive publication keeps its own checks.
pub fn execute_git_with_refresh(
    store: &mut Store,
    credential: &Credential,
    route: Route,
    protocol_v2: bool,
    source: &mut dyn Read,
    refresh: &mut dyn FnMut(&mut Store) -> Result<Credential>,
) -> Result<Output> {
    execute_git_with_cache(store, credential, route, protocol_v2, source, refresh, None)
}

pub(crate) fn execute_git_with_cache(
    store: &mut Store,
    credential: &Credential,
    route: Route,
    protocol_v2: bool,
    source: &mut dyn Read,
    refresh: &mut dyn FnMut(&mut Store) -> Result<Credential>,
    representations: Option<crate::git_representations::Context<'_>>,
) -> Result<Output> {
    let (repository, receive, advertisement) = match &route {
        Route::GitAdvertise(repository, receive) => (repository.clone(), *receive, true),
        Route::GitUpload(repository) => (repository.clone(), false, false),
        Route::GitReceive(repository) => (repository.clone(), true, false),
        _ => return Err(Error::Precondition("not a Git endpoint")),
    };
    if !receive && !protocol_v2 {
        return Err(Error::Precondition("Git fetch protocol v2 is required"));
    }
    if receive && !advertisement {
        return crate::git_receive::receive(
            store,
            credential,
            &repository,
            source,
            representations,
        );
    }
    let limits = store.limits();
    let command = command(source, advertisement, limits)?;
    let snapshot = match &command {
        Command::Advertisement => store.read_metadata_snapshot(credential, &repository, 300_000)?,
        Command::Refs(request, _) if !request.peel => {
            store.read_metadata_snapshot(credential, &repository, 300_000)?
        }
        Command::Refs(request, _) => {
            store.read_ref_snapshot(credential, &repository, 300_000, &request.ref_prefixes)?
        }
        Command::Fetch(_) => {
            store.read_snapshot(credential, &repository, 300_000, limits.graph_objects)?
        }
    };
    let result = store.with_snapshot_objects(
        credential,
        &snapshot,
        |objects| {
            read(
                objects,
                limits,
                &snapshot,
                receive,
                &command,
                representations.map(|context| context.scope(&snapshot.scope)),
            )
        },
        refresh,
    );
    // Cleanup cannot change an already-known result. A failed release leaves
    // only this bounded temporary read pin, which expires normally.
    let _ = store.release_snapshot(credential, snapshot);
    result
}
fn read(
    objects: &SnapshotObjects<'_>,
    limits: alt_remote_protocol::Limits,
    snapshot: &ReadSnapshot,
    receive: bool,
    command: &Command,
    representations: Option<crate::git_representations::Scoped<'_>>,
) -> Result<Output> {
    if matches!(command, Command::Advertisement) {
        let mut output = Vec::new();
        if receive {
            let refs: Vec<_> = snapshot
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
                    "object-format=sha1",
                    "agent=alt-server",
                ],
                HashAlgo::Sha1,
            )?;
            return Ok(bytes(
                "application/x-git-receive-pack-advertisement",
                output,
            ));
        }
        for line in ["# service=git-upload-pack\n", ""] {
            if line.is_empty() {
                pkt::write_flush(&mut output)?;
            } else {
                pkt::write_data(&mut output, line.as_bytes())?;
            }
        }
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
        return Ok(bytes("application/x-git-upload-pack-advertisement", output));
    }
    let mut remaining = limits.walk_steps;
    if let Command::Refs(request, unborn) = command {
        let tags = if request.peel {
            reference_tags(objects, snapshot, limits)?
        } else {
            BTreeMap::new()
        };
        let mut refs = snapshot.refs.clone();
        if let Some(oid) = refs.get(&snapshot.default_branch).copied() {
            refs.insert("HEAD".into(), oid);
        }
        let mut output = Vec::new();
        for (name, oid) in refs {
            if !request.ref_prefixes.is_empty()
                && !request
                    .ref_prefixes
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
            {
                continue;
            }
            let mut line = format!("{oid} {name}");
            if request.symrefs && name == "HEAD" {
                line.push_str(&format!(" symref-target:{}", snapshot.default_branch));
            }
            if request.peel && tags.contains_key(&oid) {
                line.push_str(&format!(
                    " peeled:{}",
                    peeled_reference(&tags, oid, &mut remaining)?
                ));
            }
            line.push('\n');
            pkt::write_data(&mut output, line.as_bytes())?;
        }
        if !snapshot.refs.contains_key(&snapshot.default_branch)
            && *unborn
            && (request.ref_prefixes.is_empty()
                || request
                    .ref_prefixes
                    .iter()
                    .any(|prefix| "HEAD".starts_with(prefix)))
        {
            pkt::write_data(
                &mut output,
                format!("unborn HEAD symref-target:{}\n", snapshot.default_branch).as_bytes(),
            )?;
        }
        pkt::write_flush(&mut output)?;
        return Ok(bytes(UPLOAD_RESULT, output));
    }
    let Command::Fetch(request) = command else {
        return Err(Error::Integrity("Git command dispatch"));
    };
    let roots: Vec<_> = snapshot.refs.values().copied().collect();
    let graph = verify_graph(objects, HashAlgo::Sha1, &roots, limits.into())?;
    if request.wants.is_empty() {
        return Err(Error::Integrity("empty Git wants"));
    }
    let authorized = closure(&graph, roots, &mut remaining)?;
    if request.wants.iter().any(|oid| !authorized.contains(oid)) {
        return Err(Error::Unauthorized);
    }
    let haves: Vec<_> = request
        .haves
        .iter()
        .copied()
        .filter(|oid| authorized.contains(oid))
        .collect();
    if !request.done {
        let mut output = Vec::new();
        pkt::write_data(&mut output, b"acknowledgments\n")?;
        if haves.is_empty() {
            pkt::write_data(&mut output, b"NAK\n")?;
        } else {
            for oid in haves {
                pkt::write_data(&mut output, format!("ACK {oid}\n").as_bytes())?;
            }
        }
        pkt::write_flush(&mut output)?;
        return Ok(bytes(UPLOAD_RESULT, output));
    }
    let known = closure(&graph, haves, &mut remaining)?;
    let mut selected = closure(&graph, request.wants.iter().copied(), &mut remaining)?;
    if request.include_tag {
        for (name, oid) in &snapshot.refs {
            if name.starts_with("refs/tags/")
                && selected.contains(&peeled(&graph, *oid, &mut remaining)?)
            {
                selected.extend(closure(&graph, [*oid], &mut remaining)?);
            }
        }
    }
    selected.retain(|oid| !known.contains(oid));
    let temp = tempfile::tempdir()?;
    let mut writer = alt_git_pack::PackWriter::create_with_deltas(
        temp.path(),
        HashAlgo::Sha1,
        u32::try_from(selected.len()).map_err(|_| Error::Limit("Git output count"))?,
        alt_git_pack::DeltaOptions::default(),
    )
    .map_err(pack_error)?;
    for oid in selected {
        let mut object = objects
            .open(oid)?
            .ok_or(Error::Integrity("selected read object unavailable"))?;
        let kind = object.kind;
        let size = object.bytes;
        if size > limits.object_bytes {
            return Err(Error::Limit("Git output object"));
        }
        let mut data = Vec::new();
        Read::by_ref(&mut object.reader)
            .take(size.saturating_add(1))
            .read_to_end(&mut data)?;
        if data.len() as u64 != size || ObjectId::hash_object(HashAlgo::Sha1, kind, &data) != oid {
            return Err(Error::Integrity("Git output object identity"));
        }
        if let Some(representation) = representations.and_then(|cache| cache.get(oid, &data)) {
            writer
                .add_reusing(oid, kind, &data, &representation)
                .map_err(pack_error)?;
        } else {
            writer.add(oid, kind, &data).map_err(pack_error)?;
        }
    }
    let pack = writer.finish().map_err(pack_error)?;
    let mut pack = std::fs::File::open(pack.pack_path)?;
    let mut output = tempfile::NamedTempFile::new()?;
    pkt::write_data(&mut output, b"packfile\n")?;
    let mut chunk = [0u8; 65516];
    chunk[0] = 1;
    let mut budget = limits
        .total_bytes
        .checked_mul(2)
        .ok_or(Error::Limit("Git output budget"))?;
    loop {
        let n = pack.read(&mut chunk[1..])?;
        if n == 0 {
            break;
        }
        budget = budget
            .checked_sub(n as u64 + 5)
            .ok_or(Error::Limit("Git output wire bytes"))?;
        pkt::write_data(&mut output, &chunk[..n + 1])?;
    }
    pkt::write_flush(&mut output)?;
    output.flush()?;
    let size = output.as_file().metadata()?.len();
    output.seek(SeekFrom::Start(0))?;
    Ok(Output::Temporary {
        content_type: UPLOAD_RESULT,
        bytes: size,
        file: output,
    })
}
