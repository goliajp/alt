use super::*;

impl NativeRepo<'_> {
    pub fn candidate_command(
        &mut self,
        op: &crate::cli::CandidateOp,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use crate::cli::CandidateOp;
        match op {
            CandidateOp::Create {
                task: task_id,
                parent,
            } => {
                self.ensure_writable("candidate create")?;
                let mut store = self.task_store()?;
                let task = self.require_task(&store, task_id, true)?;
                if let Some(parent) = parent {
                    let candidate: Candidate = store.load(parent)?;
                    if candidate.task != *task_id
                        || !self.task_request_complete(&candidate.creator_request)?
                    {
                        return Err(refused(
                            "parent candidate is not a completed candidate of this task",
                        ));
                    }
                }
                self.task_check_paths(&task)?;
                let manifest = self.task_manifest(&task)?;
                {
                    let source = self.task_index()?;
                    let root = alt_input::Root::open(&self.root)?;
                    for entry in &manifest.entries {
                        let artifact = match entry.node.kind {
                            alt_input::EntryKind::Directory => continue,
                            alt_input::EntryKind::Symlink => store.put_blob(&alt_input::unhex(
                                entry.node.link_target.as_deref().expect("validated link"),
                            )?)?,
                            alt_input::EntryKind::File => match entry.role {
                                alt_input::InputRole::Source => {
                                    store.put_blob(&source.bytes(&entry.path)?)?
                                }
                                alt_input::InputRole::Auxiliary => {
                                    use std::io::Seek;
                                    let mut file = tempfile::tempfile()?;
                                    if root.copy(&entry.path, &mut file)? != entry.node {
                                        return Err(refused(
                                            "auxiliary input changed while snapshotting",
                                        ));
                                    }
                                    file.rewind()?;
                                    store.capture(&mut file)?
                                }
                            },
                        };
                        if artifact.blake3 != entry.node.content || artifact.bytes != entry.node.len
                        {
                            return Err(refused(format!(
                                "input changed while snapshotting: {}",
                                entry.path
                            )));
                        }
                    }
                }
                if self.task_manifest(&task)? != manifest {
                    return Err(refused(
                        "source or auxiliary input changed during candidate capture",
                    ));
                }
                let manifest_id = store.publish(&manifest)?;
                let entries = index_entries(&self.index()?);
                let tree = write_tree(&mut self.store.odb, &entries, self.store.algo)?;
                self.store.odb.flush()?;
                let candidate = Candidate {
                    schema_version: 1,
                    task: task_id.clone(),
                    parent: parent.clone(),
                    tree: tree.to_string(),
                    source_selection: "index".into(),
                    manifest: manifest_id,
                    target_expected: task.target_expected.clone(),
                    creator_request: self.task_request()?,
                };
                let id = store.publish(&candidate)?;
                let operation = self.task_operation(task_id, "candidate-create", &id)?;
                emit(
                    out,
                    json_mode,
                    json!({"candidate_id":id,"candidate":candidate,"operation_id":operation}),
                )
            }
            CandidateOp::Show { id } => {
                let store = self.task_store()?;
                let candidate: Candidate = store.load(id)?;
                let manifest: alt_input::Manifest = store.load(&candidate.manifest)?;
                let complete = self.task_request_complete(&candidate.creator_request)?;
                emit(
                    out,
                    json_mode,
                    json!({"candidate_id":id,"candidate":candidate,"input_manifest":manifest,"creation_state":if complete {"completed"} else {"unknown"}}),
                )
            }
            CandidateOp::Check { id } => {
                let store = self.task_store()?;
                let candidate: Candidate = store.load(id)?;
                let task = self.require_task(&store, &candidate.task, true)?;
                let current = self.task_manifest(&task)?.id()?;
                let targets = self.task_targets(&candidate.target_expected)?;
                let complete = self.task_request_complete(&candidate.creator_request)?;
                let valid = complete
                    && current == candidate.manifest
                    && targets == candidate.target_expected;
                emit(
                    out,
                    json_mode,
                    json!({"candidate_id":id,"current_manifest_id":current,"recorded_manifest_id":candidate.manifest,"current_target":targets,"current":valid,"creation_complete":complete}),
                )
            }
        }
    }
}
