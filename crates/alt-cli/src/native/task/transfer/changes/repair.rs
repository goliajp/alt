use super::*;
use std::collections::BTreeSet;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Creation {
    version: u32,
    repair: String,
    scope: Scope,
    intent: String,
    creator_request: String,
    supersedes: Option<Predecessor>,
}

fn read(path: &Path) -> Res<(Creation, Vec<u8>, usize)> {
    let raw = bounded(path, 8192)?;
    let current: Creation = serde_json::from_slice(&raw)?;
    let mut bytes = raw.clone();
    let mut requests = BTreeSet::new();
    for depth in 1..=64 {
        let value: Creation = serde_json::from_slice(&bytes)?;
        alt_task::check_id(&value.repair)?;
        alt_task::check_id(&value.intent)?;
        crate::request::parse_id(&value.creator_request)?;
        if value.version != 1
            || value.repair != current.repair
            || value.scope != current.scope
            || value.intent != current.intent
            || !requests.insert(value.creator_request.clone())
        {
            return Err(refused("repair creation history mismatch"));
        }
        let Some(prior) = value.supersedes else {
            return Ok((current, raw, depth));
        };
        alt_task::check_id(&prior.digest)?;
        bytes = bounded(
            &path
                .parent()
                .expect("creation parent")
                .join("history")
                .join(format!("{}.json", prior.digest)),
            8192,
        )?;
        let previous: Creation = serde_json::from_slice(&bytes)?;
        if alt_hash::blake3::hash(&bytes).to_hex().as_str() != prior.digest
            || previous.creator_request != prior.request
        {
            return Err(refused("repair creation predecessor changed"));
        }
    }
    Err(refused(
        "repair creation history exceeds the supported limit",
    ))
}

impl NativeRepo<'_> {
    fn completed_repair(&self, creation: &Creation) -> Res<Option<serde_json::Value>> {
        let directory = self
            .store
            .alt_dir
            .join("requests")
            .join(&creation.creator_request);
        let intent: crate::request::Intent = json_file(&directory.join("intent.json"))?;
        if intent.version != 1
            || intent.hash_algorithm != "blake3"
            || intent.request_id != creation.creator_request
            || intent.repository != creation.scope.repository
            || intent.workspace != creation.scope.workspace
        {
            return Err(refused("repair creation command binding mismatch"));
        }
        let Some((completion, response)) = crate::request::read_completion(&directory, &intent)?
        else {
            return Ok(None);
        };
        if completion.outcome != "applied" || response.exit_code != 0 {
            return Ok(None);
        }
        let result: serde_json::Value = serde_json::from_slice(&response.stdout)?;
        let task: Task = serde_json::from_value(result["task"].clone())?;
        let source = task
            .spec
            .repair_source
            .as_ref()
            .ok_or_else(|| refused("completed repair has no source"))?;
        if task.creator_request != creation.creator_request
            || task.scope != creation.scope
            || source.request.digest()? != creation.repair
            || result["task_id"] != task.id()?
            || !self.task_record_complete(&task)?
        {
            return Err(refused("completed repair task differs from its creation"));
        }
        Ok(Some(result))
    }

    pub(super) fn repair_create(
        &mut self,
        op: &crate::cli::ChangeOp,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let crate::cli::ChangeOp::RepairCreate {
            remote,
            digest,
            definition,
            target,
            reconcile_request,
        } = op
        else {
            unreachable!()
        };
        self.ensure_writable("task remote changes repair-create")?;
        alt_task::check_id(digest)?;
        let binding = self.remote_binding(remote)?;
        let client = transport(&binding)?;
        check_connection(&client, &binding, true)?;
        if let Some(claim) = client.repair_claim(&binding.scope, digest)? {
            return emit(
                out,
                json_mode,
                json!({"repair_request_digest":digest,"published_task":claim,"created":false}),
            );
        }
        let source =
            client.repair_source(&binding.scope, digest, &key_bytes(&binding.receipt_key)?)?;
        let link::Body::RepairRequest(request) = &source.request.event.body else {
            return Err(refused("source is not a repair request"));
        };
        if request.target_ref != *target {
            return Err(refused(
                "task target differs from the explicit repair request",
            ));
        }
        let mut spec: alt_task::TaskSpec = serde_json::from_str(definition)
            .map_err(|e| crate::machine::Failure::usage(e.to_string()))?;
        if spec.repair_source.is_some() {
            return Err(refused(
                "repair-create supplies the verified source; omit repair_source from the definition",
            ));
        }
        spec.repair_source = Some(Box::new(source));
        spec.validate()?;
        let intent = alt_hash::blake3::hash(&serde_json::to_vec(&(&binding.scope, &spec, target))?)
            .to_hex()
            .to_string();
        let scope = self.task_scope()?;
        let creator_request = self.task_request()?;
        let path = self
            .index_path
            .with_file_name("task-repairs")
            .join(format!("{digest}.json"));
        let mut original_lock = None;
        let supersedes = if path.exists() {
            let (previous, raw, depth) = read(&path)?;
            if previous.scope != scope || previous.repair != *digest || previous.intent != intent {
                return Err(refused(
                    "repair already has a different local creation intent",
                ));
            }
            if let Some(mut result) = self.completed_repair(&previous)? {
                if reconcile_request.is_some() {
                    return Err(refused("the original repair creation already completed"));
                }
                result
                    .as_object_mut()
                    .expect("task result object")
                    .remove("request_id");
                result
                    .as_object_mut()
                    .expect("task result object")
                    .remove("operation_ids");
                result["already_created"] = json!(true);
                return emit(out, json_mode, result);
            }
            if reconcile_request.as_deref() != Some(&previous.creator_request) {
                let mut failure = crate::machine::Failure::unknown(
                    "repair creation is incomplete; inspect its original request before authorizing a new creation",
                );
                failure.details = Box::new(
                    json!({"repair_request_digest":digest,"original_request":previous.creator_request}),
                );
                failure.next_action = "Inspect `alt request status <original-request>`; explicitly use repair-create --reconcile-request <original-request> under a new request to create a new Task from the current baseline";
                return Err(failure.into());
            }
            if previous.creator_request == creator_request || depth >= 64 {
                return Err(refused(
                    "repair reconciliation requires a new request within the history limit",
                ));
            }
            original_lock = Some(crate::request::try_lock_existing(
                &self.store.alt_dir,
                &previous.creator_request,
            )?);
            if self.completed_repair(&previous)?.is_some() {
                return Err(refused(
                    "the original repair creation completed before reconciliation",
                ));
            }
            let prior = Predecessor {
                digest: alt_hash::blake3::hash(&raw).to_hex().to_string(),
                request: previous.creator_request,
            };
            let history = path.parent().expect("creation parent").join("history");
            std::fs::create_dir_all(&history)?;
            let archived = history.join(format!("{}.json", prior.digest));
            if archived.exists() {
                if bounded(&archived, 8192)? != raw {
                    return Err(refused("repair creation history collision"));
                }
            } else {
                crate::durable::write(&archived, raw)?;
            }
            Some(prior)
        } else {
            if reconcile_request.is_some() {
                return Err(refused("repair has no local creation to reconcile"));
            }
            None
        };
        std::fs::create_dir_all(path.parent().expect("creation parent"))?;
        write_json(
            &path,
            &Creation {
                version: 1,
                repair: digest.clone(),
                scope,
                intent,
                creator_request,
                supersedes,
            },
        )?;
        // This is an ordinary local task creation. Only its own durable command
        // completion makes it publishable; earlier interrupted records remain.
        self.task_command(
            &crate::cli::TaskOp::Create {
                definition: serde_json::to_string(&spec)?,
                target: target.clone(),
            },
            json_mode,
            out,
        )?;
        drop(original_lock);
        Ok(())
    }
}
