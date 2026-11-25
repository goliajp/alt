//! What was received, adopted into the repository.
use super::*;

impl NativeRepo<'_> {
    pub(in crate::native::task) fn received_adopt(
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
