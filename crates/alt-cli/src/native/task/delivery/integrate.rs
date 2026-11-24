//! A reviewed task integrated into its target.
use super::*;

impl NativeRepo<'_> {
    pub(in crate::native::task) fn task_integrate(
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
        let (author, committer) = (self.author()?, self.committer()?);
        self.run_precommit()?;
        let message = if review.commit_message.ends_with('\n') {
            review.commit_message.clone()
        } else {
            format!("{}\n", review.commit_message)
        };
        let tree = ObjectId::from_hex(candidate.tree.as_bytes())?;
        let mut bytes = build_commit_bytes(
            tree,
            &[before_oid],
            &author.sig(),
            &committer.sig(),
            &message,
        );
        if let Some(signed) = self.maybe_sign_commit_bytes(&bytes)? {
            bytes = signed;
        }
        let commit = ObjectId::hash_object(self.store.algo()?, ObjectKind::Commit, &bytes);
        self.store.odb.put(commit, ObjectKind::Commit, &bytes)?;
        self.store.odb.flush_synced()?;
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
}
