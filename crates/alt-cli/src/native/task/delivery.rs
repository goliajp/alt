use super::*;
use alt_task::{Delivery, DeliveryAttempt, Integration, Review, TaskBaseline};

impl NativeRepo<'_> {
    /// Capture public provenance before the first task can put candidate-only
    /// objects in the ODB. Later tasks retain that inventory plus public roots.
    pub(super) fn prepare_task_baseline(
        &self,
        store: &alt_task::Store,
    ) -> Res<alt_delivery::Snapshot> {
        let origin_path = self.store.alt_dir.join("delivery-origin.json");
        let config = alt_delivery::config_bytes(&self.store.alt_dir)?;
        alt_delivery::validate_config(&config).map_err(|e| refused(e.to_string()))?;
        let private_branches: std::collections::BTreeSet<_> = self
            .store
            .refs
            .iter()
            .filter(|(name, _)| name.starts_with("workspaces/") && name.ends_with("/HEAD"))
            .filter_map(|(_, target)| {
                if let RefTarget::Symbolic(name) = target {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();
        let public: BTreeMap<_, _> = self
            .store
            .refs
            .iter()
            .filter(|(name, _)| {
                *name == "HEAD"
                    || (alt_delivery::public_ref(name) && !private_branches.contains(*name))
            })
            .map(|(name, target)| (name.to_owned(), alt_delivery::render(target)))
            .collect();
        let mut origin: alt_delivery::Snapshot = match std::fs::read(&origin_path) {
            Ok(bytes) => alt_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !store.list::<Task>()?.is_empty() {
                    return Err(refused(
                        "existing tasks have no captured delivery origin; use a new independent intake for reviewed delivery",
                    ));
                }
                let origin = alt_delivery::snapshot(
                    &self.store.odb,
                    public.clone(),
                    &config,
                    self.store.algo,
                )?;
                crate::durable::write(&origin_path, alt_json::to_vec(&origin)?)?;
                origin
            }
            Err(error) => return Err(error.into()),
        };
        origin.validate(&self.store.odb)?;
        // Keep original HEAD and source refs, even when the default workspace
        // moves aside to free an integration target. No workspace HEAD is added.
        for (name, value) in public {
            if name != "HEAD" {
                origin.refs.insert(name, value);
            }
        }
        let roots = origin
            .refs
            .keys()
            .map(|name| alt_delivery::resolve(&origin.refs, name))
            .collect::<alt_delivery::Result<Vec<_>>>()?;
        origin
            .objects
            .extend(alt_delivery::closure(&self.store.odb, roots)?);
        origin.config_hex = alt_input::hex(&config);
        origin.validate(&self.store.odb)?;
        let checkout = alt_delivery::files(
            &self.store.odb,
            alt_delivery::resolve(&origin.refs, "HEAD")?,
        )?;
        alt_delivery::validate_files(&checkout, &self.store.odb)
            .map_err(|error| refused(error.to_string()))?;
        Ok(origin)
    }
    fn task_baseline_record(
        &self,
        store: &alt_task::Store,
        task: &str,
    ) -> Res<(String, alt_delivery::Snapshot)> {
        let mut records = store
            .list::<TaskBaseline>()?
            .into_iter()
            .filter(|(_, record)| record.task == task);
        let record = records
            .next()
            .ok_or_else(|| refused("task has no frozen delivery baseline"))?;
        if records.next().is_some() || !self.task_record_complete(&record.1)? {
            return Err(refused("task baseline is ambiguous or incomplete"));
        }
        let snapshot: alt_delivery::Snapshot =
            alt_json::from_slice(&store.blob(&record.1.snapshot)?)?;
        snapshot.validate(&self.store.odb)?;
        Ok((record.0, snapshot))
    }
    fn eligible_review_inputs(
        &self,
        store: &alt_task::Store,
        candidate_id: &str,
        evidence_id: &str,
    ) -> Res<(Task, Candidate)> {
        let candidate: Candidate = store.load(candidate_id)?;
        let task = self.require_task(store, &candidate.task, true)?;
        let evidence: Evidence = store.load(evidence_id)?;
        if evidence.candidate != candidate_id
            || !self.task_record_complete(&candidate)?
            || !self.task_record_complete(&evidence)?
        {
            return Err(refused(
                "candidate/evidence identity or completion is not valid",
            ));
        }
        self.task_check_paths(&task)?;
        let manifest: alt_input::Manifest = store.load(&candidate.manifest)?;
        let current = self.task_manifest(&task)?.id()?;
        let targets = self.task_targets(&candidate.target_expected)?;
        let validity = alt_task::evaluate(
            &candidate,
            &manifest,
            &evidence,
            &current,
            &targets,
            |artifact| store.verify_blob(artifact),
        )?;
        if !validity.eligible {
            let mut failure = crate::machine::Failure::refusal(
                "precondition_failed",
                "reviewed delivery requires current observed evidence",
            );
            *failure.details = json!({"validity":validity});
            return Err(failure.into());
        }
        if candidate.target_expected.len() != 1
            || candidate.target_expected.values().next() != Some(&task.baseline.head)
        {
            return Err(refused(
                "integration requires a task baseline at the expected target commit; refresh the task baseline",
            ));
        }
        let index = index_entries(&self.index()?);
        // Reconstruct without changing the store; the captured source entries
        // and canonical candidate tree must describe exactly the same bytes.
        let tree = ObjectId::from_hex(candidate.tree.as_bytes())?;
        let candidate_entries = flatten_tree(&self.store.odb, tree, self.store.algo)?;
        if index != candidate_entries {
            return Err(refused(
                "candidate tree differs from the current selected index",
            ));
        }
        let delivery_entries = candidate_entries
            .iter()
            .map(|entry| {
                Ok((
                    alt_input::checked_path(entry.path.as_slice())?.to_owned(),
                    alt_delivery::FileEntry {
                        mode: entry.mode,
                        oid: entry.oid.to_string(),
                    },
                ))
            })
            .collect::<Res<BTreeMap<_, _>>>()?;
        alt_delivery::validate_files(&delivery_entries, &self.store.odb)
            .map_err(|error| refused(error.to_string()))?;
        Ok((task, candidate))
    }
    fn review_changes(&self, task: &Task) -> Res<alt_json::Value> {
        let before = self.commit_entries(ObjectId::from_hex(task.baseline.head.as_bytes())?)?;
        let before: BTreeMap<_, _> = before
            .into_iter()
            .map(|e| (e.path.to_string(), (e.mode, e.oid)))
            .collect();
        let after = self.task_index()?;
        let paths: std::collections::BTreeSet<_> =
            before.keys().chain(after.entries.keys()).cloned().collect();
        let value = |entry: Option<&(u32, ObjectId)>| -> Res<alt_json::Value> {
            match entry {
                None => Ok(alt_json::Value::Null),
                Some((mode, oid)) => {
                    let bytes = self
                        .store
                        .odb
                        .get(oid)?
                        .ok_or("review object missing")?
                        .data;
                    Ok(
                        json!({"mode":mode,"oid":oid.to_string(),"bytes_hex":alt_input::hex(&bytes),"utf8":std::str::from_utf8(&bytes).ok()}),
                    )
                }
            }
        };
        let mut changes = Vec::new();
        for path in paths {
            if before.get(&path) != after.entries.get(&path) {
                changes.push(json!({"path":path,"path_bytes_hex":alt_input::hex(path.as_bytes()),"before":value(before.get(&path))?,"after":value(after.entries.get(&path))?}));
            }
        }
        Ok(json!({"changes":changes,"truncated":false}))
    }
    pub(super) fn task_changes(
        &self,
        candidate_id: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let store = self.task_store()?;
        let candidate: Candidate = store.load(candidate_id)?;
        let task = self.require_task(&store, &candidate.task, true)?;
        if !self.task_record_complete(&candidate)?
            || self.task_manifest(&task)?.id()? != candidate.manifest
        {
            return Err(refused(
                "candidate changed or is incomplete; create a new candidate",
            ));
        }
        emit(
            out,
            json_mode,
            json!({"candidate_id":candidate_id,"target_expected":candidate.target_expected,"diff":self.review_changes(&task)?}),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn task_review(
        &mut self,
        candidate_id: &str,
        evidence_id: &str,
        reviewer: &str,
        reason: &str,
        message: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task review")?;
        let mut store = self.task_store()?;
        let (task, candidate) = self.eligible_review_inputs(&store, candidate_id, evidence_id)?;
        let (baseline, _) = self.task_baseline_record(&store, &candidate.task)?;
        let changes = store.put_blob(&alt_json::to_vec(&self.review_changes(&task)?)?)?;
        let review = Review {
            schema_version: 1,
            task: candidate.task.clone(),
            candidate: candidate_id.into(),
            evidence: evidence_id.into(),
            baseline,
            target_expected: candidate.target_expected,
            reviewer: reviewer.into(),
            reason: reason.into(),
            commit_message: message.into(),
            changes,
            creator_request: self.task_request()?,
        };
        let id = store.publish(&review)?;
        let operation = self.task_operation(&review.task, "task-review", &id)?;
        emit(
            out,
            json_mode,
            json!({"review_id":id,"review":review,"operation_id":operation}),
        )
    }
    pub(super) fn task_review_show(
        &self,
        id: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let store = self.task_store()?;
        let review: Review = store.load(id)?;
        let changes: alt_json::Value = alt_json::from_slice(&store.blob(&review.changes)?)?;
        emit(
            out,
            json_mode,
            json!({"review_id":id,"review":review,"creation_complete":self.task_record_complete(&review)?,"diff":changes}),
        )
    }
    pub(super) fn task_integrate(
        &mut self,
        review_id: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task integrate")?;
        // Hold the workspace mutation lock through the final input checks and
        // ref transaction, before taking the private task-record lock.
        self.lock_workspace()?;
        let mut store = self.task_store()?;
        let review: Review = store.load(review_id)?;
        if !self.task_record_complete(&review)? {
            return Err(refused("review creation is incomplete"));
        }
        self.store.refresh()?;
        let (task, candidate) =
            self.eligible_review_inputs(&store, &review.candidate, &review.evidence)?;
        if review.task != candidate.task
            || review.target_expected != candidate.target_expected
            || store.blob(&review.changes)? != alt_json::to_vec(&self.review_changes(&task)?)?
        {
            return Err(refused("review no longer describes this candidate/target"));
        }
        let (baseline, baseline_record) = self.task_baseline_record(&store, &review.task)?;
        if baseline != review.baseline {
            return Err(refused("review baseline differs"));
        }
        let (target, before) = review
            .target_expected
            .iter()
            .next()
            .ok_or("review target missing")?;
        let before_oid = ObjectId::from_hex(before.as_bytes())?;
        // Check occupancy and capabilities before publishing an intent; the
        // actual ref transaction checks these again under its lock.
        let refs = self
            .store
            .refs
            .iter()
            .map(|(n, t)| (n.to_owned(), t.clone()))
            .collect();
        let probe = RefChange {
            name: target.clone(),
            old: Some(RefTarget::Oid(before_oid)),
            new: Some(RefTarget::Oid(before_oid)),
        };
        workspace::one_workspace_per_branch(&self.head_ref, &refs, &[probe]).map_err(refused)?;
        let expected_notes = baseline_record.refs.get(crate::meta::NOTES_REF).cloned();
        let actual_notes = self
            .store
            .refs
            .get(crate::meta::NOTES_REF)
            .map(alt_delivery::render);
        if actual_notes != expected_notes {
            return Err(refused(
                "public commit metadata moved since the task baseline; refresh the task",
            ));
        }
        for name in [target.as_str(), crate::meta::NOTES_REF] {
            if self.caps.branch_deny.iter().any(|g| g.matches(name))
                || (!self.caps.branch_allow.is_empty()
                    && !self.caps.branch_allow.iter().any(|g| g.matches(name)))
            {
                return Err(crate::machine::Failure::refusal(
                    "capability_denied",
                    "principal cannot update the integration target and its public note",
                )
                .into());
            }
        }
        self.run_precommit()?;
        let identity = self.id.clone();
        let (name, email) = identity.sig();
        let sig = Sig {
            name,
            email,
            when: (now_ms() / 1000) as i64,
            tz: "+0000",
        };
        let message = if review.commit_message.ends_with('\n') {
            review.commit_message.clone()
        } else {
            format!("{}\n", review.commit_message)
        };
        let tree = ObjectId::from_hex(candidate.tree.as_bytes())?;
        let mut bytes = build_commit_bytes(tree, &[before_oid], &sig, &sig, &message);
        if let Some(signed) = self.maybe_sign_commit_bytes(&bytes)? {
            bytes = signed;
        }
        let commit = ObjectId::hash_object(self.store.algo, ObjectKind::Commit, &bytes);
        self.store.odb.put(commit, ObjectKind::Commit, &bytes)?;
        self.store.odb.flush()?;
        let note = self.meta_for(&message, tree, &[before_oid], &notes::MetaInput::default())?;
        let note_change = self.note_change(commit, &note)?;
        if note_change.old.as_ref().map(alt_delivery::render) != expected_notes {
            return Err(refused(
                "public commit metadata moved while preparing integration",
            ));
        }
        let notes_after = note_change
            .new
            .as_ref()
            .map(alt_delivery::render)
            .ok_or("integration note missing")?;
        // Signing may invoke external execution. Recheck the exact candidate
        // after they return, before recording any integration intent.
        self.eligible_review_inputs(&store, &review.candidate, &review.evidence)?;
        let integration = Integration {
            schema_version: 1,
            task: review.task.clone(),
            review: review_id.into(),
            commit: commit.to_string(),
            target: target.clone(),
            before: before.clone(),
            notes_before: expected_notes,
            notes_after,
            creator_request: self.task_request()?,
        };
        let id = store.publish(&integration)?;
        self.task_operation(&review.task, "task-integrate-prepare", &id)?;
        self.store.sink()?.fsync_all()?;
        let ref_operation = self.commit_refs(
            "task-integrate",
            &[
                RefChange {
                    name: target.clone(),
                    old: Some(RefTarget::Oid(before_oid)),
                    new: Some(RefTarget::Oid(commit)),
                },
                note_change,
            ],
        )?;
        let event = Event {
            schema_version: 1,
            task: review.task.clone(),
            previous: None,
            state: TaskState::Integrated,
            reason: format!("review {review_id}; integration {id}"),
            creator_request: self.task_request()?,
        };
        let event_id = store.publish(&event)?;
        let operation = self.task_operation(&review.task, "task-integrate-complete", &id)?;
        emit(
            out,
            json_mode,
            json!({"integration_id":id,"integration":integration,"event_id":event_id,"operation_id":operation,"ref_operation_id":ref_operation.to_string()}),
        )
    }
    pub(super) fn task_integration_show(
        &self,
        id: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let store = self.task_store()?;
        let integration: Integration = store.load(id)?;
        let current = self
            .store
            .refs
            .resolve(&integration.target)?
            .map(|oid| oid.to_string());
        let complete = self.integration_completed(&store, id, &integration)?;
        let operations = self
            .store
            .refs
            .oplog()
            .ops()
            .iter()
            .filter(|op| {
                requests::operation_request(op).ok().flatten().as_deref()
                    == Some(&integration.creator_request)
            })
            .map(|op| op.id.to_string())
            .collect::<Vec<_>>();
        emit(
            out,
            json_mode,
            json!({"integration_id":id,"integration":integration,"creation_complete":complete,"current_target":current,"state":if complete {"integrated"}else if self.integration_abandoned(&store,&integration)? {"abandoned"}else{"unknown"},"operation_ids":operations,"next_action":if complete {"export the immutable integration"}else{"inspect the original request and attributed ref operations; do not repeat with a new request"}}),
        )
    }
    pub(super) fn task_export(
        &mut self,
        integration_id: &str,
        target: &Path,
        git: &Path,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task export")?;
        let mut store = self.task_store()?;
        let integration: Integration = store.load(integration_id)?;
        self.require_task(&store, &integration.task, false)?;
        if !self.integration_completed(&store, integration_id, &integration)? {
            return Err(refused("integration has no successful durable completion"));
        }
        let review: Review = store.load(&integration.review)?;
        let baseline: TaskBaseline = store.load(&review.baseline)?;
        if baseline.task != integration.task || !self.task_record_complete(&review)? {
            return Err(refused("integration review/baseline mismatch"));
        }
        let snapshot: alt_delivery::Snapshot =
            alt_json::from_slice(&store.blob(&baseline.snapshot)?)?;
        let mut refs = snapshot.refs.clone();
        refs.insert(integration.target.clone(), integration.commit.clone());
        refs.insert(
            crate::meta::NOTES_REF.into(),
            integration.notes_after.clone(),
        );
        let commit = ObjectId::from_hex(integration.commit.as_bytes())?;
        let target = alt_delivery::destination(target).map_err(|e| refused(e.to_string()))?;
        let attempt = DeliveryAttempt {
            schema_version: 1,
            task: integration.task.clone(),
            integration: integration_id.into(),
            destination: target.to_str().ok_or("unsupported destination")?.into(),
            creator_request: self.task_request()?,
        };
        let attempt_id = store.publish(&attempt)?;
        self.task_operation(&integration.task, "task-export-prepare", &attempt_id)?;
        self.store.sink()?.fsync_all()?;
        drop(store);
        let prepared = alt_delivery::prepare_export(
            &self.store.odb,
            &snapshot,
            refs,
            &[commit],
            &target,
            git,
        )?;
        let mut store = self.task_store()?;
        let report_artifact = store.put_blob(&alt_json::to_vec(&prepared.report)?)?;
        let delivery = Delivery {
            schema_version: 1,
            task: integration.task.clone(),
            integration: integration_id.into(),
            destination: target
                .to_str()
                .ok_or("unsupported destination path")?
                .into(),
            report: report_artifact,
            creator_request: self.task_request()?,
        };
        let id = store.publish(&delivery)?;
        self.task_operation(&integration.task, "task-export-ready", &id)?;
        self.store.sink()?.fsync_all()?;
        let report = prepared.publish()?;
        let operation = self.task_operation(&integration.task, "task-export", &id)?;
        emit(
            out,
            json_mode,
            json!({"delivery_id":id,"delivery":delivery,"report":report,"operation_id":operation}),
        )
    }
    pub(super) fn task_delivery_show(
        &self,
        id: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let store = self.task_store()?;
        let delivery: Delivery = store.load(id)?;
        let report: alt_json::Value = alt_json::from_slice(&store.blob(&delivery.report)?)?;
        emit(
            out,
            json_mode,
            json!({"delivery_id":id,"delivery":delivery,"report":report,"creation_complete":self.delivery_completed(&store,id,&delivery)?}),
        )
    }
}

pub(crate) fn delivery_intake(
    source: &Path,
    target: &Path,
    git: &Path,
    json_mode: bool,
    out: &mut impl Write,
) -> Res<()> {
    // Resolve/preflight first. A published directory is never removed on a
    // late durability error; its intake report remains inspectable.
    let target = alt_delivery::destination(target).map_err(|e| refused(e.to_string()))?;
    let report = match alt_delivery::intake(source, &target, git) {
        Ok(report) => report,
        Err(error) if target.exists() => {
            return Err(crate::machine::Failure::unknown(format!(
                "intake publication needs inspection at {}: {error}",
                target.display()
            ))
            .into());
        }
        Err(error) => return Err(refused(error.to_string())),
    };
    emit(
        out,
        json_mode,
        json!({"destination":target,"baseline_id":report.baseline.id()?,"report":report}),
    )
}

impl NativeRepo<'_> {
    fn integration_ref_operation_with_store(
        &self,
        store: &alt_task::Store,
        integration: &Integration,
    ) -> Res<Option<String>> {
        let _: Task = store.load(&integration.task)?;
        crate::request::parse_id(&integration.creator_request)?;
        let intent: crate::request::Intent = match super::transfer_source::bounded(
            &self
                .store
                .alt_dir
                .join("requests")
                .join(&integration.creator_request)
                .join("intent.json"),
            4 * 1024 * 1024,
        ) {
            Ok(raw) => alt_json::from_slice(&raw)?,
            Err(alt_task_transfer::Error::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if intent.repository != self.task_scope()?.repository
            || intent.request_id != integration.creator_request
        {
            return Err(refused(
                "integration source intent does not belong to this repository",
            ));
        }
        let mut found = None;
        for op in self.store.refs.oplog().ops() {
            let Some(tx) = alt_refs::parse_tx(&op.payload)? else {
                continue;
            };
            let Some(context) = tx.context else { continue };
            let context: recovery::RefContext = alt_json::from_slice(&context)?;
            if !matches!(context.version, 1 | 2)
                || context.workspace.instance != intent.workspace
                || context.request_id.as_deref() != Some(&integration.creator_request)
            {
                continue;
            }
            if tx.changes.len() != 2 {
                continue;
            }
            let branch = tx.changes.iter().find(|c| c.name == integration.target);
            let notes = tx.changes.iter().find(|c| c.name == crate::meta::NOTES_REF);
            let matches = branch.is_some_and(|c| {
                c.old.as_ref().map(alt_delivery::render).as_deref() == Some(&integration.before)
                    && c.new.as_ref().map(alt_delivery::render).as_deref()
                        == Some(&integration.commit)
            }) && notes.is_some_and(|c| {
                c.old.as_ref().map(alt_delivery::render) == integration.notes_before
                    && c.new.as_ref().map(alt_delivery::render).as_deref()
                        == Some(&integration.notes_after)
            });
            if matches {
                if found.is_some() {
                    return Err(crate::machine::Failure::unknown(
                        "integration has multiple attributed target transactions",
                    )
                    .into());
                }
                found = Some(op.id.to_string());
            }
        }
        Ok(found)
    }
    pub(super) fn integration_completed(
        &self,
        store: &alt_task::Store,
        id: &str,
        integration: &Integration,
    ) -> Res<bool> {
        if self.received_record_complete(&alt_task_service::RecordKey {
            kind: alt_task_service::RecordKind::Integration,
            id: id.into(),
        })? {
            return Ok(true);
        }
        let Some(operation) = self.integration_ref_operation_with_store(store, integration)? else {
            return Ok(false);
        };
        if self.task_record_complete(integration)? {
            return Ok(true);
        }
        for (_, receipt) in store.list::<alt_task::IntegrationReceipt>()? {
            if receipt.integration == id
                && receipt.ref_operation == operation
                && self.task_record_complete(&receipt)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn task_integration_collect(
        &mut self,
        id: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task integration-collect")?;
        let mut store = self.task_store()?;
        let integration: Integration = store.load(id)?;
        self.require_task(&store, &integration.task, false)?;
        let operation=self.integration_ref_operation_with_store(&store,&integration)?.ok_or_else(||refused("no matching attributed ref transaction; collection cannot infer application or execute the intent"))?;
        let receipt = alt_task::IntegrationReceipt {
            schema_version: 1,
            integration: id.into(),
            ref_operation: operation,
            creator_request: self.task_request()?,
        };
        let receipt_id = store.publish(&receipt)?;
        let operation =
            self.task_operation(&integration.task, "task-integration-collect", &receipt_id)?;
        emit(
            out,
            json_mode,
            json!({"integration_id":id,"receipt_id":receipt_id,"receipt":receipt,"operation_id":operation,"state":"integrated","executed_again":false}),
        )
    }
}

impl NativeRepo<'_> {
    /// Hold the original request claim while proving that an interrupted
    /// integration did not apply. A pending/running process is never guessed.
    pub(super) fn prepare_task_abandon(
        &mut self,
        store: &alt_task::Store,
        task_id: &str,
    ) -> Res<Option<std::fs::File>> {
        self.require_task(store, task_id, false)?;
        let integrations: Vec<_> = store
            .list::<Integration>()?
            .into_iter()
            .filter(|(_, i)| i.task == task_id)
            .collect();
        if integrations.len() > 1 {
            return Err(crate::machine::Failure::unknown(
                "multiple integration intents require inspection",
            )
            .into());
        }
        let Some((_, integration)) = integrations.first() else {
            return Ok(None);
        };
        crate::request::parse_id(&integration.creator_request)?;
        let path = self
            .store
            .alt_dir
            .join("requests")
            .join(&integration.creator_request)
            .join("lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        if alt_os::sys::flock(&file, alt_os::sys::LOCK_EX | alt_os::sys::LOCK_NB).is_err() {
            return Err(refused(
                "integration request is still active; query it before abandonment",
            ));
        }
        self.store.refresh()?;
        if self
            .integration_ref_operation_with_store(store, integration)?
            .is_some()
        {
            return Err(refused(
                "integration already applied; collect its receipt instead of abandoning it",
            ));
        }
        Ok(Some(file))
    }
    pub(super) fn integration_abandoned(
        &self,
        store: &alt_task::Store,
        integration: &Integration,
    ) -> Res<bool> {
        if self
            .integration_ref_operation_with_store(store, integration)?
            .is_some()
        {
            return Ok(false);
        }
        for (_, event) in store.list::<Event>()? {
            if event.task == integration.task
                && event.state == TaskState::Abandoned
                && self.task_record_complete(&event)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl NativeRepo<'_> {
    fn delivery_completed(
        &self,
        store: &alt_task::Store,
        id: &str,
        delivery: &Delivery,
    ) -> Res<bool> {
        if self.task_record_complete(delivery)? {
            return Ok(true);
        }
        for (_, receipt) in store.list::<alt_task::DeliveryReceipt>()? {
            if receipt.delivery == id && self.task_record_complete(&receipt)? {
                store.verify_blob(&receipt.verification)?;
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn task_delivery_collect(
        &mut self,
        id: &str,
        git: &Path,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task delivery-collect")?;
        let store = self.task_store()?;
        let delivery: Delivery = store.load(id)?;
        self.require_task(&store, &delivery.task, false)?;
        let integration: Integration = store.load(&delivery.integration)?;
        if !self.integration_completed(&store, &delivery.integration, &integration)? {
            return Err(refused("integration completion is still unknown"));
        }
        let report: alt_delivery::ExportReport =
            alt_json::from_slice(&store.blob(&delivery.report)?)?;
        drop(store);
        let verification = alt_delivery::verify_existing(
            &self.store.odb,
            &report,
            Path::new(&delivery.destination),
            git,
        )
        .map_err(|e| refused(e.to_string()))?;
        let mut store = self.task_store()?;
        let artifact = store.put_blob(&alt_json::to_vec(&verification)?)?;
        let receipt = alt_task::DeliveryReceipt {
            schema_version: 1,
            delivery: id.into(),
            verification: artifact,
            creator_request: self.task_request()?,
        };
        let receipt_id = store.publish(&receipt)?;
        let operation =
            self.task_operation(&delivery.task, "task-delivery-collect", &receipt_id)?;
        emit(
            out,
            json_mode,
            json!({"delivery_id":id,"receipt_id":receipt_id,"receipt":receipt,"verification":verification,"operation_id":operation,"exported_again":false}),
        )
    }
}
