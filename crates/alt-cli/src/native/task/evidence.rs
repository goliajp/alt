use super::*;

impl NativeRepo<'_> {
    pub fn evidence_command(
        &mut self,
        op: &crate::cli::EvidenceOp,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use crate::cli::EvidenceOp;
        match op {
            EvidenceOp::Run { candidate, profile } => {
                self.evidence_run(candidate, profile, json_mode, out)
            }
            EvidenceOp::Collect { id } => self.evidence_collect(id, json_mode, out),
            EvidenceOp::RunStatus { id } => self.evidence_run_status(id, json_mode, out),
            EvidenceOp::Import {
                candidate: candidate_id,
                report,
                stdout,
                stderr,
            } => {
                self.ensure_writable("evidence import")?;
                let mut store = self.task_store()?;
                let candidate: Candidate = store.load(candidate_id)?;
                self.require_task(&store, &candidate.task, true)?;
                if !self.task_record_complete(&candidate)? {
                    return Err(crate::machine::Failure::unknown(
                        "candidate creation has no successful completion",
                    )
                    .into());
                }
                let mut evidence: Evidence = alt_json::from_str(report)
                    .map_err(|error| crate::machine::Failure::usage(error.to_string()))?;
                if evidence.candidate != *candidate_id || evidence.manifest != candidate.manifest {
                    return Err(refused(
                        "external report names a different candidate or input manifest",
                    ));
                }
                evidence.validate()?;
                evidence.original_report = Some(store.put_blob(report.as_bytes())?);
                // The original report preserves claimed provenance and hashes.
                // Imported data cannot grant itself observed-run authority.
                evidence.provenance = alt_task::Provenance::Reported;
                evidence.creator_request = self.task_request()?;
                evidence.stdout = stdout
                    .as_ref()
                    .map(|path| -> Res<Artifact> {
                        Ok(store.capture(&mut std::fs::File::open(path)?)?)
                    })
                    .transpose()?;
                evidence.stderr = stderr
                    .as_ref()
                    .map(|path| -> Res<Artifact> {
                        Ok(store.capture(&mut std::fs::File::open(path)?)?)
                    })
                    .transpose()?;
                let id = store.publish(&evidence)?;
                let operation = self.task_operation(&candidate.task, "evidence-import", &id)?;
                emit(
                    out,
                    json_mode,
                    json!({"evidence_id":id,"evidence":evidence,"eligible":false,"reason":"external_report_unobserved","operation_id":operation}),
                )
            }
            EvidenceOp::Show { id } => {
                let store = self.task_store()?;
                let evidence: Evidence = store.load(id)?;
                let complete = self.task_record_complete(&evidence)?;
                emit(
                    out,
                    json_mode,
                    json!({"evidence_id":id,"evidence":evidence,"creation_state":if complete {"completed"} else {"unknown"}}),
                )
            }
            EvidenceOp::Check { id } => {
                let store = self.task_store()?;
                let evidence: Evidence = store.load(id)?;
                let candidate: Candidate = store.load(&evidence.candidate)?;
                let task = self.require_task(&store, &candidate.task, true)?;
                let manifest: alt_input::Manifest = store.load(&candidate.manifest)?;
                let current = self.task_manifest(&task)?.id()?;
                let targets = self.task_targets(&candidate.target_expected)?;
                let mut validity = alt_task::evaluate(
                    &candidate,
                    &manifest,
                    &evidence,
                    &current,
                    &targets,
                    |artifact| store.verify_blob(artifact),
                )?;
                if !self.task_record_complete(&candidate)?
                    || !self.task_record_complete(&evidence)?
                {
                    validity.eligible = false;
                    validity.reasons.push("creation_completion_unknown".into());
                }
                emit(
                    out,
                    json_mode,
                    json!({"evidence_id":id,"candidate_id":evidence.candidate,"validity":validity}),
                )
            }
            EvidenceOp::Log { id, stream } => {
                let store = self.task_store()?;
                let evidence: Evidence = store.load(id)?;
                let artifact = match stream.as_str() {
                    "stdout" => evidence.stdout,
                    "stderr" => evidence.stderr,
                    "original-report" => evidence.original_report,
                    _ => {
                        return Err(
                            crate::machine::Failure::usage("unknown evidence stream").into()
                        );
                    }
                }
                .ok_or_else(|| refused("evidence stream is missing"))?;
                let bytes = store.blob(&artifact)?;
                if json_mode {
                    emit(
                        out,
                        true,
                        json!({"evidence_id":id,"stream":stream,"artifact":artifact,"encoding":"hex","bytes_hex":alt_input::hex(&bytes),"truncated":false}),
                    )
                } else {
                    out.write_all(&bytes)?;
                    Ok(())
                }
            }
        }
    }
}
