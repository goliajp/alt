//! A publication previewed, sent and received.
use super::*;

impl NativeRepo<'_> {
    pub(super) fn publication_preview(
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
    pub(super) fn publication_send(
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
    pub(super) fn publication_receive(
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
        let odb = &self.store.odb;
        let unavailable = |_| alt_repository_service::Error::Integrity("local object unavailable");
        let held = |oid| {
            Ok(odb
                .lookup(&oid)
                .map_err(unavailable)?
                .map(|entry| entry.kind))
        };
        let received = Received::verify_into(&package, task_limits(), &held)?;
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
        self.store.odb.flush_synced()?;
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
}
