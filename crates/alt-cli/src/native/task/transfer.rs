use super::transfer_source::{LocalSource, bounded};
use super::*;
use alt_remote_protocol as protocol;
use alt_task_service::{RecordKey, TaskLimits, TaskRecord};
use alt_task_transfer::{Package, Received};
use alt_wire_http::native::{Options, Transport, verify_receipt};
use std::io::{Read, Seek, SeekFrom};
mod adoption;
mod changes;
mod current_review;
use adoption::{Adoption, Predecessor};

fn limits() -> protocol::Limits {
    protocol::Limits {
        request_bytes: 8 * 1024 * 1024,
        ref_updates: 1000,
        record_roots: 1000,
        manifest_entries: 100_000,
        graph_objects: 100_000,
        graph_edges: 1_000_000,
        structured_bytes: 4 * 1024 * 1024,
        tree_paths: 1_000_000,
        path_bytes: 4096,
        walk_steps: 10_000_000,
        chunk_bytes: 1024 * 1024,
        object_bytes: 256 * 1024 * 1024,
        total_bytes: 4 * 1024 * 1024 * 1024,
    }
}
fn task_limits() -> TaskLimits {
    TaskLimits {
        objects: 100_000,
        records: 10_000,
        edges: 100_000,
        structured_bytes: 4 * 1024 * 1024,
        total_bytes: 4 * 1024 * 1024 * 1024,
        work_bytes: 32 * 1024 * 1024 * 1024,
        artifact_bytes: 256 * 1024 * 1024,
        paths: 1_000_000,
        path_bytes: 4096,
        walk_steps: 10_000_000,
    }
}
#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct Binding {
    version: u32,
    origin: String,
    scope: protocol::Scope,
    receipt_key: String,
    actor: protocol::PrincipalInfo,
    local_repository: String,
    allow_loopback_http: bool,
    transfer_limits: protocol::Limits,
}
fn negotiated_limits(remote: protocol::Limits) -> protocol::Limits {
    let mut local = limits();
    macro_rules! bound { ($($field:ident),*)=>{$(local.$field=local.$field.min(remote.$field);)*}; }
    bound!(
        request_bytes,
        ref_updates,
        record_roots,
        manifest_entries,
        graph_objects,
        graph_edges,
        structured_bytes,
        tree_paths,
        path_bytes,
        walk_steps,
        chunk_bytes,
        object_bytes,
        total_bytes
    );
    local
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct PublishedOrigin {
    version: u32,
    binding: Binding,
    receipt: protocol::Receipt,
    #[json(default, skip_serializing_if = "Vec::is_empty")]
    provenance: Vec<protocol::TransferProof>,
}
#[derive(alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct Plan {
    selection: alt_task_transfer::Selection,
    #[json(default)]
    refs: Vec<protocol::RefUpdate>,
}

fn key_bytes(key: &str) -> Res<[u8; 32]> {
    alt_task::check_id(key)?;
    alt_input::unhex(key)?
        .try_into()
        .map_err(|_| refused("receipt public key must be 32 bytes"))
}
fn identifier(value: &str) -> Res<()> {
    protocol::Scope {
        installation_id: value.into(),
        repository_instance_id: value.into(),
        recovery_epoch: 1,
    }
    .validate()?;
    Ok(())
}
fn kind(value: &str) -> Res<protocol::RecordKind> {
    match value {
        "task" => Ok(protocol::RecordKind::Task),
        "candidate" => Ok(protocol::RecordKind::Candidate),
        "evidence" => Ok(protocol::RecordKind::Evidence),
        "review" => Ok(protocol::RecordKind::Review),
        "integration" => Ok(protocol::RecordKind::Integration),
        "delivery" => Ok(protocol::RecordKind::Delivery),
        _ => Err(crate::machine::Failure::usage(
            "kind must be task, candidate, evidence, review, integration or delivery",
        )
        .into()),
    }
}
fn json_file<T: alt_json::FromJson>(path: &Path) -> Res<T> {
    Ok(alt_json::from_slice(&bounded(
        path,
        limits().request_bytes,
    )?)?)
}
fn write_json(path: &Path, value: &impl alt_json::ToJson) -> Res<()> {
    Ok(crate::durable::write(path, alt_json::to_vec(value)?)?)
}
fn transport(binding: &Binding) -> Res<Transport> {
    let token = if let Some(path) = std::env::var_os("ALT_SERVER_CREDENTIAL") {
        if std::env::var_os("ALT_SERVER_TOKEN").is_some() {
            return Err(refused(
                "set only one of ALT_SERVER_CREDENTIAL or ALT_SERVER_TOKEN",
            ));
        }
        crate::auth::credential_for_origin(Path::new(&path), &binding.origin)?
    } else {
        std::env::var("ALT_SERVER_TOKEN").map_err(|_| {
            refused("set ALT_SERVER_CREDENTIAL to the private file saved by alt auth login")
        })?
    };
    Ok(Transport::new(
        &binding.origin,
        token,
        Options {
            timeout: std::time::Duration::from_secs(60),
            response_bytes: limits().request_bytes,
            chunk_bytes: limits().chunk_bytes,
            content_bytes: limits().object_bytes,
            allow_loopback_http: binding.allow_loopback_http,
        },
    )?)
}
fn check_connection(client: &Transport, binding: &Binding, write: bool) -> Res<()> {
    let capability = client.capabilities()?;
    capability.capabilities.require_task_bundles()?;
    if capability.receipt_public_key != binding.receipt_key {
        return Err(refused(
            "server receipt key changed; inspect the instance before binding a new remote",
        ));
    }
    let repository = client.repository(&binding.scope.repository_instance_id)?;
    if repository.scope != binding.scope || repository.object_format != protocol::ObjectFormat::Sha1
    {
        return Err(refused(
            "repository identity or recovery epoch changed; query the original instance history",
        ));
    }
    if write {
        let principal = client.principal()?;
        if principal.actor_id != binding.actor.actor_id
            || principal.attribution != binding.actor.attribution
        {
            return Err(refused(
                "credential no longer matches the previewed actor and delegation",
            ));
        }
    }
    Ok(())
}
fn observed(
    observation: protocol::RequestObservation,
    publication: &protocol::Publication,
) -> Res<Option<protocol::RequestState>> {
    let expected = &publication.key;
    match observation {
        protocol::RequestObservation::Recorded { key, result } if &key == expected => {
            if result.digest() != publication.digest(limits())? {
                return Err(refused("server response belongs to a different intent"));
            }
            Ok(Some(*result))
        }
        protocol::RequestObservation::NotSeen { key } if &key == expected => Ok(None),
        protocol::RequestObservation::Unknown { key } if &key == expected => {
            Err(crate::machine::Failure::unknown(
                "server cannot yet confirm the original publication outcome",
            )
            .into())
        }
        _ => Err(refused("server response request identity mismatch")),
    }
}
fn remote_error(
    error: alt_wire_http::native::Error,
    key: &protocol::RequestKey,
) -> Box<dyn std::error::Error> {
    // A transport/authentication failure is not a durable publication result:
    // an earlier attempt may already have committed. Only an authenticated
    // Recorded terminal state can establish rejection/expiry.
    let mut failure = crate::machine::Failure::unknown(error.to_string());
    failure.details = Box::new(json!({"remote_request":key}));
    failure.next_action =
        "Run `alt task remote status <publication-id>` before resuming the same publication";
    failure.into()
}

impl NativeRepo<'_> {
    /// Check a specific immutable record, never just a possibly colliding
    /// request ID from another repository/worktree.
    pub(super) fn task_record_complete<T: Record>(&self, record: &T) -> Res<bool> {
        let id = record.id()?;
        let key = RecordKey {
            kind: alt_json::from_value(json!(T::KIND))?,
            id,
        };
        let raw = alt_json::to_vec(
            &json!({"schema_version":1,"hash_algorithm":"blake3","kind":T::KIND,"record":record}),
        )?;
        let decoded = TaskRecord::decode(&key, &raw, task_limits().structured_bytes)?;
        if let TaskRecord::Evidence(evidence) = &decoded
            && self.local_collected_evidence_complete(evidence)?
        {
            return Ok(true);
        }
        match super::transfer_source::completed(
            self,
            &key,
            &decoded,
            task_limits().structured_bytes,
        ) {
            Ok(_) => Ok(true),
            Err(alt_task_transfer::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                self.received_record_complete(&key)
            }
            Err(error) => Err(error.into()),
        }
    }
    pub(super) fn received_record_complete(&self, key: &RecordKey) -> Res<bool> {
        for package in self.received_packages()? {
            let received = Received::verify(&package, task_limits())?;
            if received.bundle().records.iter().any(|r| &r.record == key) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn task_adopted(&self, id: &str) -> Res<bool> {
        alt_task::check_id(id)?;
        if !self.adoption_path(id).exists() {
            return Ok(false);
        }
        let (adoption, _, _) = self.read_adoption(id)?;
        if !self.adoption_applied(&adoption)? {
            return Ok(false);
        }
        for package in self.received_packages()? {
            if package.request().publication.digest(limits())? == adoption.publication {
                let received = Received::verify(&package, task_limits())?;
                return Ok(received.bundle().records.iter().any(|r| {
                    r.record.kind == alt_task_service::RecordKind::Task && r.record.id == id
                }));
            }
        }
        Err(refused("adopted task's received publication is missing"))
    }
    fn remote_binding(&self, name: &str) -> Res<Binding> {
        identifier(name)?;
        let binding: Binding = json_file(
            &self
                .store
                .alt_dir
                .join("task-remotes")
                .join(format!("{name}.json")),
        )?;
        self.check_binding(&binding)?;
        Ok(binding)
    }
    fn check_binding(&self, binding: &Binding) -> Res<()> {
        binding.scope.validate()?;
        binding.transfer_limits.validate()?;
        if negotiated_limits(binding.transfer_limits) != binding.transfer_limits {
            return Err(refused("saved transfer limits exceed client bounds"));
        }
        key_bytes(&binding.receipt_key)?;
        if binding.version != 1 || binding.local_repository != self.task_scope()?.repository {
            return Err(refused(
                "server binding belongs to another local repository",
            ));
        }
        Ok(())
    }
    pub(super) fn received_packages(&self) -> Res<Vec<Package>> {
        let directory = self.store.alt_dir.join("received-tasks");
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                return Err(refused("invalid received history directory"));
            }
            if !entry.path().join("origin.json").exists() {
                continue;
            }
            paths.push(entry.path());
            if paths.len() > 1024 {
                return Err(refused(
                    "received publication inventory exceeds client limit",
                ));
            }
        }
        paths.sort();
        let mut packages = Vec::new();
        for path in paths {
            let origin: PublishedOrigin = json_file(&path.join("origin.json"))?;
            self.check_binding(&origin.binding)?;
            let package = Package::open(&path.join("sealed"), limits())?;
            let publication = &package.request().publication;
            if origin.version != if origin.provenance.is_empty() { 1 } else { 2 }
                || path.file_name().and_then(|n| n.to_str())
                    != Some(publication.digest(limits())?.as_str())
            {
                return Err(refused("received publication origin mismatch"));
            }
            alt_provenance::verify_origin(
                publication,
                &origin.receipt,
                &origin.provenance,
                &origin.binding.scope,
                &key_bytes(&origin.binding.receipt_key)?,
                limits(),
            )?;
            packages.push(package);
        }
        Ok(packages)
    }
    pub(super) fn task_remote_command(
        &mut self,
        op: &crate::cli::TaskRemoteOp,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use crate::cli::TaskRemoteOp;
        match op {
            TaskRemoteOp::Changes { op } => self.change_command(op, json_mode, out),
            TaskRemoteOp::Connect {
                name,
                url,
                repository,
                receipt_key,
                allow_loopback_http,
            } => {
                self.ensure_writable("task remote connect")?;
                identifier(name)?;
                identifier(repository)?;
                key_bytes(receipt_key)?;
                let directory = self.store.alt_dir.join("task-remotes");
                std::fs::create_dir_all(&directory)?;
                let path = directory.join(format!("{name}.json"));
                if path.exists() {
                    return Err(refused(
                        "remote binding already exists; use a new explicit name for another identity",
                    ));
                }
                let mut binding = Binding {
                    version: 1,
                    origin: url.clone(),
                    scope: protocol::Scope {
                        installation_id: "pending".into(),
                        repository_instance_id: repository.clone(),
                        recovery_epoch: 1,
                    },
                    receipt_key: receipt_key.clone(),
                    actor: protocol::PrincipalInfo {
                        actor_id: "pending".into(),
                        attribution: protocol::Attribution {
                            kind: protocol::ActorKind::Human,
                            on_behalf_of: None,
                            delegation_id: None,
                            agent_session: None,
                        },
                    },
                    local_repository: self.task_scope()?.repository,
                    allow_loopback_http: *allow_loopback_http,
                    transfer_limits: limits(),
                };
                let client = transport(&binding)?;
                let caps = client.capabilities()?;
                caps.capabilities.require_task_bundles()?;
                if caps.receipt_public_key != *receipt_key {
                    return Err(refused(
                        "server receipt key does not match the supplied trusted key",
                    ));
                }
                binding.transfer_limits = negotiated_limits(caps.capabilities.limits);
                let info = client.repository(repository)?;
                if info.object_format != protocol::ObjectFormat::Sha1 {
                    return Err(refused("unsupported remote object format"));
                }
                binding.scope = info.scope;
                binding.actor = client.principal()?;
                binding.origin = client.origin().into();
                write_json(&path, &binding)?;
                emit(
                    out,
                    json_mode,
                    json!({"remote":name,"origin":binding.origin,"scope":binding.scope,"actor":binding.actor,"receipt_key":binding.receipt_key}),
                )
            }
            TaskRemoteOp::Preview { remote, plan } => {
                self.publication_preview(remote, plan, json_mode, out)
            }
            TaskRemoteOp::Publish { id, prepare_only } => {
                self.publication_send(id, false, *prepare_only, json_mode, out)
            }
            TaskRemoteOp::Status { id } => self.publication_send(id, true, false, json_mode, out),
            TaskRemoteOp::Attest { evidence, key_file } => {
                self.collector_attest(evidence, key_file, json_mode, out)
            }
            TaskRemoteOp::Review {
                remote,
                actor,
                request,
            } => self.remote_review(remote, actor, request, json_mode, out),
            TaskRemoteOp::Approve {
                remote,
                actor,
                request,
                integration,
                digest,
            } => self.remote_approve(remote, actor, request, integration, digest, json_mode, out),
            TaskRemoteOp::List {
                remote,
                kind: record_kind,
                id,
                after,
            } => {
                let binding = self.remote_binding(remote)?;
                let client = transport(&binding)?;
                check_connection(&client, &binding, false)?;
                let kind = kind(record_kind)?;
                let value = if let Some(id) = id {
                    alt_json::to_value(client.bundles(
                        &binding.scope,
                        &protocol::RecordRoot {
                            kind,
                            id: id.clone(),
                        },
                        after.as_deref(),
                    )?)?
                } else {
                    alt_json::to_value(client.records(&binding.scope, kind, after.as_deref())?)?
                };
                emit(
                    out,
                    json_mode,
                    json!({"remote":remote,"scope":binding.scope,"items":value}),
                )
            }
            TaskRemoteOp::Receive {
                remote,
                kind: record_kind,
                id,
                publication,
            } => self.publication_receive(
                remote,
                protocol::RecordRoot {
                    kind: kind(record_kind)?,
                    id: id.clone(),
                },
                publication,
                json_mode,
                out,
            ),
            TaskRemoteOp::Adopt {
                task,
                publication,
                reconcile_request,
            } => self.received_adopt(
                task,
                publication,
                reconcile_request.as_deref(),
                json_mode,
                out,
            ),
        }
    }
    fn publication_preview(
        &mut self,
        remote: &str,
        plan: &Path,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task remote preview")?;
        let binding = self.remote_binding(remote)?;
        let client = transport(&binding)?;
        check_connection(&client, &binding, true)?;
        let plan: Plan = json_file(&self.root.join(plan))?;
        let id = self.task_request()?;
        let publication = protocol::Publication {
            protocol_version: 1,
            key: protocol::RequestKey {
                scope: binding.scope.clone(),
                actor_id: binding.actor.actor_id.clone(),
                request_id: id.clone(),
            },
            attribution: binding.actor.attribution.clone(),
            object_format: protocol::ObjectFormat::Sha1,
            refs: plan.refs,
            record_roots: vec![],
            content_manifest: "0".repeat(64),
        };
        let tasks = self.task_store()?;
        let packages = self.received_packages()?;
        let received = packages
            .iter()
            .map(|package| Received::verify(package, task_limits()))
            .collect::<alt_task_transfer::Result<Vec<_>>>()?;
        let source = LocalSource {
            repo: self,
            tasks: &tasks,
            received: &received,
        };
        let parent = self.store.alt_dir.join("task-publications");
        std::fs::create_dir_all(&parent)?;
        let (draft, publication) = alt_task_transfer::build(
            &parent,
            &source,
            plan.selection,
            publication,
            binding.transfer_limits,
            task_limits(),
        )?;
        let package = draft.seal(publication, &parent.join(&id), task_limits())?;
        write_json(&parent.join(&id).join("binding.json"), &binding)?;
        let verified = Received::verify(&package, task_limits())?;
        emit(
            out,
            json_mode,
            json!({"publication_id":id,"request_digest":package.request().publication.digest(limits())?,"remote":binding.origin,"publication":package.request().publication,"manifest":package.request().manifest,"task_selection":verified.bundle(),"state":"previewed"}),
        )
    }
    fn publication_send(
        &mut self,
        id: &str,
        query_only: bool,
        prepare_only: bool,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        crate::request::parse_id(id)?;
        if !query_only {
            self.ensure_writable("task remote publish")?;
        }
        let directory = self.store.alt_dir.join("task-publications").join(id);
        let binding: Binding = json_file(&directory.join("binding.json"))?;
        self.check_binding(&binding)?;
        let package = Package::open(&directory.join("sealed"), limits())?;
        package.verify(task_limits())?;
        let publication = &package.request().publication;
        let key = &publication.key;
        if key.request_id != id
            || key.scope != binding.scope
            || key.actor_id != binding.actor.actor_id
            || publication.attribution != binding.actor.attribution
        {
            return Err(refused("publication preview binding mismatch"));
        }
        let client = transport(&binding)?;
        check_connection(&client, &binding, !query_only)?;
        let observation = client.query(key).map_err(|e| remote_error(e, key))?;
        let mut state = observed(observation.clone(), publication)?;
        if query_only {
            if let Some(protocol::RequestState::Committed { receipt }) = &state {
                verify_receipt(
                    receipt,
                    publication,
                    &key_bytes(&binding.receipt_key)?,
                    limits(),
                )?;
            }
            return emit(
                out,
                json_mode,
                json!({"publication_id":id,"observation":observation}),
            );
        }
        if state.is_none() {
            state = Some(
                client
                    .begin(package.request(), limits())
                    .map_err(|e| remote_error(e, key))?,
            );
        }
        if let Some(protocol::RequestState::Receiving { .. }) = &state {
            let uploaded: std::collections::BTreeSet<_> = client
                .upload_status(key)
                .map_err(|e| remote_error(e, key))?
                .verified_chunks
                .into_iter()
                .collect();
            for entry in &package.request().manifest.entries {
                let mut file = package.content(&entry.blake3)?;
                for chunk in &entry.chunks {
                    if uploaded.contains(&chunk.blake3) {
                        continue;
                    }
                    file.seek(SeekFrom::Start(chunk.offset))?;
                    let mut bytes = vec![0; usize::try_from(chunk.bytes)?];
                    file.read_exact(&mut bytes)?;
                    client
                        .upload_chunk(key, chunk, &bytes)
                        .map_err(|e| remote_error(e, key))?;
                }
                client
                    .assemble(key, &entry.blake3)
                    .map_err(|e| remote_error(e, key))?;
            }
            state = observed(
                client.prepare(key).map_err(|e| remote_error(e, key))?,
                publication,
            )?;
        }
        if let Some(protocol::RequestState::Ready { .. }) = &state {
            if prepare_only {
                return emit(
                    out,
                    json_mode,
                    json!({"publication_id":id,"state":"ready","request":key,"request_digest":publication.digest(limits())?}),
                );
            }
            state = observed(
                client.publish(key).map_err(|e| remote_error(e, key))?,
                publication,
            )?;
        }
        match state {
            Some(protocol::RequestState::Committed { receipt }) => {
                verify_receipt(
                    &receipt,
                    publication,
                    &key_bytes(&binding.receipt_key)?,
                    limits(),
                )?;
                write_json(&directory.join("receipt.json"), &receipt)?;
                emit(
                    out,
                    json_mode,
                    json!({"publication_id":id,"state":"committed","receipt":receipt}),
                )
            }
            Some(protocol::RequestState::Rejected { failure, .. }) => Err(refused(format!(
                "publication rejected: {:?}; inspect its original request",
                failure.code
            ))),
            Some(protocol::RequestState::Expired { .. }) => Err(refused(
                "publication expired; inspect and create a new preview explicitly",
            )),
            _ => Err(crate::machine::Failure::unknown(
                "publication remains unresolved; query the same publication ID",
            )
            .into()),
        }
    }
    fn publication_receive(
        &mut self,
        remote: &str,
        root: protocol::RecordRoot,
        digest: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task remote receive")?;
        alt_task::check_id(digest)?;
        root.validate()?;
        let binding = self.remote_binding(remote)?;
        let client = transport(&binding)?;
        check_connection(&client, &binding, false)?;
        let mut after = None;
        let mut found = None;
        for _ in 0..1024 {
            let items = client.bundles(&binding.scope, &root, after.as_deref())?;
            if items.is_empty() {
                break;
            }
            for item in items {
                let current = item.publication.digest(limits())?;
                if after.as_ref().is_some_and(|previous| previous >= &current) {
                    return Err(refused("server publication pagination did not advance"));
                }
                after = Some(current.clone());
                if current == digest {
                    found = Some(item);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let selected = found.ok_or_else(|| {
            refused("selected signed publication is not available for this record")
        })?;
        selected
            .manifest
            .validate_publication(&selected.publication, limits())?;
        alt_provenance::verify_origin(
            &selected.publication,
            &selected.receipt,
            &selected.provenance,
            &binding.scope,
            &key_bytes(&binding.receipt_key)?,
            limits(),
        )?;
        let parent = self.store.alt_dir.join("received-tasks");
        std::fs::create_dir_all(&parent)?;
        let directory = parent.join(digest);
        let package = if directory.exists() {
            let package = Package::open(&directory.join("sealed"), limits())?;
            if package.request().publication.digest(limits())? != digest {
                return Err(refused(
                    "existing received package has a different identity",
                ));
            }
            package
        } else {
            let mut draft = alt_task_transfer::Draft::new(&parent, limits())?;
            for entry in &selected.manifest.entries {
                let mut file = alt_os::temp::tempfile_in(&parent)?;
                client.download(&binding.scope, &entry.blake3, entry.bytes, &mut file)?;
                file.rewind()?;
                draft.capture_selected(entry, &mut file)?;
            }
            draft.seal(selected.publication.clone(), &directory, task_limits())?
        };
        let received = Received::verify(&package, task_limits())?;
        let key = alt_task_transfer::root_key(&root);
        if !received.bundle().records.iter().any(|r| r.record == key) {
            return Err(refused("publication does not contain the requested record"));
        }
        let mut tasks = alt_task::Store::open(&self.store.alt_dir.join("task-data"))?;
        received.retain_records(&mut tasks)?;
        for oid in received.verified().graph.objects() {
            let object = alt_repository_service::ObjectSource::open(&package, oid)?
                .ok_or_else(|| refused("received object missing"))?;
            let mut bytes = Vec::new();
            object
                .reader
                .take(object.bytes.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 != object.bytes {
                return Err(refused("received object changed"));
            }
            self.store.odb.put(oid, object.kind, &bytes)?;
        }
        self.store.odb.flush()?;
        write_json(
            &directory.join("origin.json"),
            &PublishedOrigin {
                version: if selected.provenance.is_empty() { 1 } else { 2 },
                binding,
                receipt: selected.receipt,
                provenance: selected.provenance,
            },
        )?;
        emit(
            out,
            json_mode,
            json!({"publication_digest":digest,"received":received.bundle().records,"omissions":received.bundle().omissions,"state":"received","execution":"none"}),
        )
    }
    fn received_adopt(
        &mut self,
        id: &str,
        publication: &str,
        reconcile_request: Option<&str>,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task remote adopt")?;
        alt_task::check_id(id)?;
        alt_task::check_id(publication)?;
        if self.workspace == "default" || self.root.join(".git").exists() {
            return Err(refused(
                "task adoption requires a named isolated native workspace",
            ));
        }
        let packages = self.received_packages()?;
        let package = packages
            .iter()
            .find(|p| p.request().publication.digest(limits()).ok().as_deref() == Some(publication))
            .ok_or_else(|| refused("publication has no verified local received origin"))?;
        let received = Received::verify(package, task_limits())?;
        let key = RecordKey {
            kind: alt_task_service::RecordKind::Task,
            id: id.into(),
        };
        let TaskRecord::Task(task) = received
            .verified()
            .tasks
            .as_ref()
            .expect("received tasks")
            .prepared()
            .record(&key)?
        else {
            return Err(refused("selected record is not a task"));
        };
        if !received.bundle().omissions.is_empty() {
            return Err(refused(
                "received publication has omitted attachments; receive a complete selection before adopting",
            ));
        }
        if self.task_targets(&task.target_expected)? != task.target_expected {
            return Err(refused(
                "task integration targets differ from their original expected state",
            ));
        }
        if self
            .store
            .refs
            .resolve(&self.head_branch()?)?
            .map(|o| o.to_string())
            .as_deref()
            != Some(task.baseline.head.as_str())
        {
            return Err(refused(
                "workspace HEAD must match the original task baseline before adoption",
            ));
        }
        if task.target_expected.contains_key(&self.head_branch()?) {
            return Err(refused(
                "task workspace branch must be separate from integration targets",
            ));
        }
        self.task_check_paths(task)?;
        let destination = self.task_scope()?;
        let creator_request = self.task_request()?;
        let path = self.adoption_path(id);
        let mut original_lock = None;
        let supersedes = if path.exists() {
            let Some(original_request) = reconcile_request else {
                return Err(refused(
                    "this workspace already has an adoption record for the task; inspect its original request",
                ));
            };
            let (previous, raw, depth) = self.read_adoption(id)?;
            if previous.creator_request != original_request
                || original_request == creator_request
                || previous.publication != publication
            {
                return Err(refused(
                    "reconciliation must name the current adoption's exact original request and publication, under a new request",
                ));
            }
            original_lock = Some(crate::request::try_lock_existing(
                &self.store.alt_dir,
                original_request,
            )?);
            if self.adoption_applied(&previous)? {
                return Err(refused(
                    "the original adoption already completed successfully",
                ));
            }
            if depth >= 64 {
                return Err(refused(
                    "task adoption history exceeds the supported chain limit",
                ));
            }
            let predecessor = Predecessor {
                digest: alt_hash::blake3::hash(&raw).to_hex().to_string(),
                request: original_request.into(),
            };
            self.preserve_adoption(id, &raw, &predecessor)?;
            Some(predecessor)
        } else {
            if reconcile_request.is_some() {
                return Err(refused("task has no adoption record to reconcile"));
            }
            None
        };
        std::fs::create_dir_all(path.parent().expect("adoption parent"))?;
        let adoption = Adoption {
            version: if supersedes.is_some() { 2 } else { 1 },
            task: id.into(),
            publication: publication.into(),
            destination,
            creator_request,
            supersedes,
        };
        write_json(&path, &adoption)?;
        let action = if adoption.supersedes.is_some() {
            "task-adopt-reconcile"
        } else {
            "task-adopt"
        };
        let operation = self.task_operation(id, action, publication)?;
        emit(
            out,
            json_mode,
            json!({"adopted_task_id":id,"publication_digest":publication,"destination":adoption.destination,"original_scope":task.scope,"operation_id":operation,"supersedes":adoption.supersedes}),
        )?;
        drop(original_lock);
        Ok(())
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_fixture_tests!("native-task-transfer"; Binding, PublishedOrigin);
}
