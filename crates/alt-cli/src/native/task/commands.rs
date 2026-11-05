use super::*;

impl NativeRepo<'_> {
    pub fn task_command(
        &mut self,
        op: &crate::cli::TaskOp,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use crate::cli::TaskOp;
        match op {
            TaskOp::Remote { op } => self.task_remote_command(op, json_mode, out),
            TaskOp::Changes { candidate } => self.task_changes(candidate, json_mode, out),
            TaskOp::Review {
                candidate,
                evidence,
                reviewer,
                reason,
                message,
            } => self.task_review(
                candidate, evidence, reviewer, reason, message, json_mode, out,
            ),
            TaskOp::ReviewShow { id } => self.task_review_show(id, json_mode, out),
            TaskOp::Integrate { review } => self.task_integrate(review, json_mode, out),
            TaskOp::IntegrationShow { id } => self.task_integration_show(id, json_mode, out),
            TaskOp::IntegrationCollect { id } => self.task_integration_collect(id, json_mode, out),
            TaskOp::Export {
                integration,
                target,
                git,
            } => self.task_export(integration, target, git, json_mode, out),
            TaskOp::DeliveryShow { id } => self.task_delivery_show(id, json_mode, out),
            TaskOp::DeliveryCollect { id, git } => {
                self.task_delivery_collect(id, git, json_mode, out)
            }
            TaskOp::Create { definition, target } => {
                self.ensure_writable("task create")?;
                alt_input::check_execution_platform(&self.root)?;
                alt_input::check_execution_platform(&self.store.alt_dir)?;
                if self.root.join(".git").exists()
                    || self
                        .store
                        .alt_dir
                        .parent()
                        .is_some_and(|root| root.join(".git").exists())
                {
                    return Err(refused(
                        "task execution requires a native repository without a coexisting .git",
                    ));
                }
                if self.workspace == "default" {
                    return Err(refused(
                        "task creation requires a named isolated workspace; create one with alt workspace add",
                    ));
                }
                let spec: alt_task::TaskSpec = serde_json::from_str(definition)
                    .map_err(|error| crate::machine::Failure::usage(error.to_string()))?;
                spec.validate()?;
                if !target.starts_with("refs/heads/") {
                    return Err(refused("task target must be a full local branch ref"));
                }
                let head = self
                    .store
                    .refs
                    .resolve(&self.head_branch()?)?
                    .ok_or_else(|| refused("task baseline requires a commit"))?;
                let target_oid = self
                    .store
                    .refs
                    .resolve(target)?
                    .ok_or_else(|| refused("task target does not exist"))?;
                if target == &self.head_branch()? {
                    return Err(refused(
                        "task workspace branch must be separate from its integration target",
                    ));
                }
                let baseline = alt_task::Baseline {
                    head: head.to_string(),
                    tree: self.commit_tree(head)?.to_string(),
                    refs: self
                        .store
                        .refs
                        .iter()
                        .filter(|(name, _)| name.starts_with("refs/"))
                        .map(|(name, value)| (name.to_owned(), render_target(&Some(value.clone()))))
                        .collect(),
                };
                let task = Task {
                    schema_version: 1,
                    scope: self.task_scope()?,
                    spec,
                    baseline,
                    target_expected: BTreeMap::from([(target.clone(), target_oid.to_string())]),
                    creator_request: self.task_request()?,
                };
                self.task_check_paths(&task)?;
                // Validate the current selection before creating task metadata.
                self.task_manifest(&task)?;
                let mut store = alt_task::Store::open(&self.store.alt_dir.join("task-data"))?;
                let snapshot = self.prepare_task_baseline(&store)?;
                let id = store.publish(&task)?;
                let snapshot = store.put_blob(&serde_json::to_vec(&snapshot)?)?;
                let baseline_id = store.publish(&alt_task::TaskBaseline {
                    schema_version: 1,
                    task: id.clone(),
                    snapshot,
                    creator_request: self.task_request()?,
                })?;
                let operation = self.task_operation(&id, "task-create", &id)?;
                emit(
                    out,
                    json_mode,
                    json!({"task_id":id,"task":task,"baseline_id":baseline_id,"operation_id":operation}),
                )
            }
            TaskOp::List => {
                if !self.store.alt_dir.join("task-data").exists() {
                    return emit(out, json_mode, json!({"tasks":[]}));
                }
                let store = self.task_store()?;
                let mut tasks = Vec::new();
                for (id, task) in store.list::<Task>()? {
                    let complete = self.task_record_complete(&task)?;
                    let state = self.task_state(&store, &id, &task)?;
                    tasks.push(json!({"task_id":id,"task":task,"state":state,"creation_state":if complete {"completed"} else {"unknown"}}));
                }
                emit(out, json_mode, json!({"tasks":tasks}))
            }
            TaskOp::Show { id } => {
                let store = self.task_store()?;
                let task: Task = store.load(id)?;
                let complete = self.task_record_complete(&task)?;
                let state = self.task_state(&store, id, &task)?;
                let mut events = Vec::new();
                for (event_id, event) in store.list::<Event>()? {
                    if &event.task == id {
                        let completed = self.task_record_complete(&event)?;
                        events.push(json!({"event_id":event_id,"event":event,"request_state":if completed {"completed"} else {"unknown"}}));
                    }
                }
                emit(
                    out,
                    json_mode,
                    json!({"task_id":id,"task":task,"state":state,"creation_state":if complete {"completed"} else {"unknown"},"events":events}),
                )
            }
            TaskOp::Abandon { id, reason } => {
                self.ensure_writable("task abandon")?;
                let mut store = self.task_store()?;
                let _integration_guard = self.prepare_task_abandon(&store, id)?;
                let events = store.list::<Event>()?;
                if events.iter().any(|(_, event)| &event.task == id) {
                    return Err(refused(
                        "task already has a terminal or uncertain state event",
                    ));
                }
                let event = Event {
                    schema_version: 1,
                    task: id.clone(),
                    previous: None,
                    state: TaskState::Abandoned,
                    reason: reason.clone(),
                    creator_request: self.task_request()?,
                };
                let event_id = store.publish(&event)?;
                let operation = self.task_operation(id, "task-abandon", &event_id)?;
                emit(
                    out,
                    json_mode,
                    json!({"task_id":id,"event_id":event_id,"event":event,"operation_id":operation}),
                )
            }
        }
    }
}
