use super::*;
use alt_task::{RunAttempt, RunReceipt};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

mod transport;

#[derive(serde::Deserialize, alt_json::FromJson)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[json(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Profile {
    Local {
        scratch: PathBuf,
        tools: Vec<alt_runner::ToolBinding>,
        timeout_ms: u64,
    },
    RemoteSync {
        scratch: PathBuf,
        tools: Vec<alt_runner::ToolBinding>,
        timeout_ms: u64,
        device: String,
        sync_program: PathBuf,
        observer: String,
        observer_blake3: String,
        max_wait_seconds: u64,
    },
}
impl Profile {
    fn common(&self) -> (&Path, &[alt_runner::ToolBinding], u64) {
        match self {
            Self::Local {
                scratch,
                tools,
                timeout_ms,
            }
            | Self::RemoteSync {
                scratch,
                tools,
                timeout_ms,
                ..
            } => (scratch, tools, *timeout_ms),
        }
    }
}

fn captured(store: &mut alt_task::Store, path: &Path) -> Res<Artifact> {
    Ok(store.capture(&mut fs::File::open(path)?)?)
}

impl NativeRepo<'_> {
    // Launch/collection bookkeeping remains local and is not imported as task
    // execution history. Bind a receipt to the actual completed response.
    pub(super) fn run_receipt_complete(&self, receipt: &RunReceipt) -> Res<bool> {
        crate::request::parse_id(&receipt.creator_request)?;
        let directory = self
            .store
            .alt_dir
            .join("requests")
            .join(&receipt.creator_request);
        let intent = match std::fs::read(directory.join("intent.json")) {
            Ok(raw) => serde_json::from_slice::<crate::request::Intent>(&raw)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let Some((completion, response)) = crate::request::read_completion(&directory, &intent)?
        else {
            return Ok(false);
        };
        if completion.outcome != "applied" {
            return Ok(false);
        }
        if intent.repository != self.task_scope()?.repository
            || intent.request_id != receipt.creator_request
        {
            return Err(refused(
                "run collection completion does not bind this receipt",
            ));
        }
        // Human-mode failed validation predates machine failure details. The
        // original completed operation still binds the exact local receipt;
        // it is never exported as a fabricated JSON source response.
        if let Ok(value) = alt_task_service::command_record_result(
            &response,
            &receipt.creator_request,
            4 * 1024 * 1024,
        ) && (value["receipt_id"] != receipt.id()?
            || value["evidence_id"] != receipt.evidence
            || value["attempt_id"] != receipt.attempt)
        {
            return Err(refused("run collection response record mismatch"));
        }
        for op in self.store.refs.oplog().ops() {
            if op.payload.first() != Some(&TASK_OPERATION)
                || !completion.operations.contains(&op.id.to_string())
            {
                continue;
            }
            let operation: TaskOperation = recovery::decode(&op.payload, TASK_OPERATION)?;
            if operation.version == 1
                && operation.workspace.instance == intent.workspace
                && operation.request_id.as_deref() == Some(&receipt.creator_request)
                && operation.action == "evidence-run-collect"
                && operation.record_id == receipt.id()?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn local_collected_evidence_complete(&self, evidence: &Evidence) -> Res<bool> {
        let id = evidence.id()?;
        // Read without reopening the store lock already held by the caller.
        for op in self.store.refs.oplog().ops() {
            if op.payload.first() != Some(&TASK_OPERATION) {
                continue;
            }
            let operation: TaskOperation = recovery::decode(&op.payload, TASK_OPERATION)?;
            if operation.action != "evidence-run-collect"
                || operation.request_id.as_deref() != Some(&evidence.creator_request)
            {
                continue;
            }
            alt_task::check_id(&operation.record_id)?;
            let path = self
                .store
                .alt_dir
                .join("task-data/records/run-receipt")
                .join(format!("{}.json", operation.record_id));
            let raw = super::transfer_source::bounded(&path, 4 * 1024 * 1024)?;
            let value: serde_json::Value = serde_json::from_slice(&raw)?;
            if value["schema_version"] != 1
                || value["hash_algorithm"] != "blake3"
                || value["kind"] != "run-receipt"
            {
                return Err(refused("invalid local run receipt envelope"));
            }
            let receipt: RunReceipt = serde_json::from_value(value["record"].clone())?;
            if receipt.id()? != operation.record_id {
                return Err(refused("local run receipt address mismatch"));
            }
            if receipt.evidence == id
                && receipt.creator_request == evidence.creator_request
                && self.run_receipt_complete(&receipt)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn evidence_run(
        &mut self,
        candidate_id: &str,
        definition: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("evidence run")?;
        let profile: Profile = serde_json::from_str(definition)
            .map_err(|error| crate::machine::Failure::usage(error.to_string()))?;
        let mut store = self.task_store()?;
        let candidate: Candidate = store.load(candidate_id)?;
        let task = self.require_task(&store, &candidate.task, true)?;
        if !self.task_record_complete(&candidate)? {
            return Err(
                crate::machine::Failure::unknown("candidate creation is not complete").into(),
            );
        }
        let receipts = store.list::<RunReceipt>()?;
        for (id, attempt) in store.list::<RunAttempt>()? {
            if attempt.task == candidate.task {
                let mut finished = false;
                for (_, receipt) in &receipts {
                    if receipt.attempt == id && self.run_receipt_complete(receipt)? {
                        finished = true;
                    }
                }
                if !finished {
                    let mut failure = crate::machine::Failure::unknown(
                        "an earlier run has no collected result; inspect run-status and collect it before another launch",
                    );
                    *failure.details = json!({"attempt_id":id});
                    return Err(failure.into());
                }
            }
        }
        self.store.refresh()?;
        if self.task_manifest(&task)?.id()? != candidate.manifest
            || self.task_targets(&candidate.target_expected)? != candidate.target_expected
        {
            return Err(refused(
                "candidate inputs or target changed before validation",
            ));
        }
        let manifest: alt_input::Manifest = store.load(&candidate.manifest)?;
        alt_runner::output_roots(&manifest)?;
        let (scratch, tools, timeout_ms) = profile.common();
        if !scratch.is_absolute() || timeout_ms == 0 {
            return Err(crate::machine::Failure::usage(
                "runner scratch must be absolute and timeout_ms positive",
            )
            .into());
        }
        let scratch = fs::canonicalize(scratch)?;
        alt_input::check_execution_platform(&scratch)?;
        if scratch
            .ancestors()
            .any(|parent| parent.join(".git").exists() || parent.join(".alt").exists())
        {
            return Err(refused(
                "runner scratch must be outside existing repositories",
            ));
        }
        if self.store.algo != HashAlgo::Sha1 {
            return Err(refused("runner source snapshot currently requires SHA-1"));
        }
        // Request IDs are scoped to a repository/workspace; transport paths
        // share a host namespace. Give this launch its own private nonce so
        // equal caller IDs from independent repositories cannot overwrite it.
        let nonce = crate::machine::hex(&crate::request::new_id()?);
        let root = scratch.join(format!("alt-run-{nonce}"));
        fs::create_dir(&root)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        alt_runner::materialize(&root, &manifest, |digest, bytes| {
            store
                .blob(&Artifact {
                    blake3: digest.into(),
                    bytes,
                })
                .map_err(|error| alt_runner::Error::Invalid(error.to_string()))
        })?;
        // A real native source snapshot lets the canonical sync tool use its
        // ordinary tracked/ignored rules. Auxiliary bytes never enter its ODB.
        super::super::init(Some(root.clone()), &mut std::io::sink())?;
        let mut snapshot = super::super::Store::open(root.join(".alt"))?;
        let source = self.task_index()?;
        for (_, oid) in source.entries.values() {
            let object = self.store.odb.get(oid)?.ok_or("source object missing")?;
            snapshot.odb.put(*oid, ObjectKind::Blob, &object.data)?;
        }
        snapshot.odb.flush()?;
        save_index(&root.join(".alt/index"), &self.index()?, self.store.algo)?;
        drop(snapshot);
        let control = root.join(alt_runner::CONTROL);
        fs::create_dir(&control)?;
        fs::create_dir(control.join("auxiliary"))?;
        for input in &manifest.plan.auxiliary {
            if input.path.contains('/') {
                continue;
            }
            if let Some(entry) = manifest.entries.iter().find(|entry| {
                entry.path == input.path && entry.node.kind != alt_input::EntryKind::Directory
            }) {
                fs::write(
                    control.join("auxiliary").join(&entry.node.content),
                    store.blob(&Artifact {
                        blake3: entry.node.content.clone(),
                        bytes: entry.node.len,
                    })?,
                )?;
            }
        }
        let spec = alt_runner::Spec {
            schema_version: 1,
            candidate: candidate_id.into(),
            nonce: nonce.clone(),
            manifest,
            tools: tools.to_vec(),
            timeout_ms,
            runner: match &profile {
                Profile::Local { .. } => "local".into(),
                Profile::RemoteSync { device, .. } => device.clone(),
            },
        };
        fs::write(control.join("spec.json"), serde_json::to_vec(&spec)?)?;
        let spec_artifact = captured(&mut store, &control.join("spec.json"))?;
        let (remote, observer_path, observer) =
            transport::prepare(&root, &profile, &spec, &mut store)?;
        let mut authentication_key = [0u8; 32];
        use std::io::Read;
        fs::File::open("/dev/urandom")?.read_exact(&mut authentication_key)?;
        let key_artifact = store.put_blob(&authentication_key)?;
        let attempt = RunAttempt {
            schema_version: 1,
            task: candidate.task.clone(),
            candidate: candidate_id.into(),
            manifest: candidate.manifest.clone(),
            nonce,
            local_directory: root.to_str().ok_or("unsupported scratch path")?.into(),
            remote,
            observer_path,
            observer,
            spec: spec_artifact,
            authentication_key: key_artifact,
            creator_request: self.task_request()?,
        };
        let attempt_id = store.publish(&attempt)?;
        self.task_operation(&candidate.task, "evidence-run-start", &attempt_id)?;
        // A daemon store can defer normal command durability. Launching an
        // external process needs the attribution durable before that boundary.
        self.store.odb.flush()?;
        self.store.sink()?.fsync_all()?;
        // Status readers and unrelated tasks must remain available while the
        // external runner is active. Collection takes the private store again.
        drop(store);
        // The observer writes an atomic report before acknowledging completion.
        // A transport failure is never answered by another execution.
        if let Err(error) = transport::launch(&attempt, &authentication_key) {
            let mut failure = crate::machine::Failure::unknown(format!(
                "run launch/transport did not acknowledge completion: {error}"
            ));
            *failure.details =
                json!({"attempt_id":attempt_id,"local_directory":attempt.local_directory});
            return Err(failure.into());
        }
        self.evidence_collect(&attempt_id, json_mode, out)
    }

    pub(super) fn evidence_run_status(
        &self,
        id: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let store = self.task_store()?;
        let attempt: RunAttempt = store.load(id)?;
        let mut receipts = Vec::new();
        for (receipt_id, receipt) in store.list::<RunReceipt>()? {
            if receipt.attempt == id {
                let complete = self.run_receipt_complete(&receipt)?;
                receipts.push(json!({"receipt_id":receipt_id,"receipt":receipt,"collection_complete":complete}));
            }
        }
        emit(
            out,
            json_mode,
            json!({"attempt_id":id,"attempt":attempt,"receipts":receipts,"next_action":"Collect only an existing observer report; never re-execute this nonce"}),
        )
    }

    pub(super) fn evidence_collect(
        &mut self,
        id: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("evidence collect")?;
        let mut store = self.task_store()?;
        let attempt: RunAttempt = store.load(id)?;
        let task = self.require_task(&store, &attempt.task, true)?;
        let candidate: Candidate = store.load(&attempt.candidate)?;
        if candidate.task != attempt.task || candidate.manifest != attempt.manifest {
            return Err(refused("attempt/candidate identity mismatch"));
        }
        let (mut evidence, report) = transport::collect(&attempt, &mut store)?;
        evidence.creator_request = self.task_request()?;
        evidence.original_report = Some(report);
        let evidence_id = store.publish(&evidence)?;
        let receipt = RunReceipt {
            schema_version: 1,
            attempt: id.into(),
            evidence: evidence_id.clone(),
            creator_request: self.task_request()?,
        };
        let receipt_id = store.publish(&receipt)?;
        let operation = self.task_operation(&attempt.task, "evidence-run-collect", &receipt_id)?;
        self.store.refresh()?;
        let manifest: alt_input::Manifest = store.load(&candidate.manifest)?;
        let current = self.task_manifest(&task)?.id()?;
        let target = self.task_targets(&candidate.target_expected)?;
        let validity = alt_task::evaluate(
            &candidate,
            &manifest,
            &evidence,
            &current,
            &target,
            |artifact| store.verify_blob(artifact),
        )?;
        let result = json!({"attempt_id":id,"evidence_id":evidence_id,"receipt_id":receipt_id,"operation_id":operation,"evidence":evidence,"validity":validity});
        if !validity.eligible {
            let mut failure = crate::machine::Failure::refusal(
                "validation_not_eligible",
                "validation evidence was preserved but is not eligible",
            );
            failure.outcome = "applied";
            failure.operation_id = Some(operation);
            if evidence.termination == alt_task::Termination::Unknown {
                failure.exit_code = 75;
            }
            *failure.details = result;
            return Err(failure.into());
        }
        emit(out, json_mode, result)
    }
}
