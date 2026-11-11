use super::*;

#[derive(serde::Serialize, serde::Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct ReviewContext {
    binding: Binding,
    selection: protocol::ReviewSelection,
}

impl NativeRepo<'_> {
    pub(super) fn collector_attest(
        &self,
        id: &str,
        key_file: &Path,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use std::os::unix::fs::PermissionsExt;
        let store = self.task_store()?;
        let evidence: Evidence = store.load(id)?;
        if evidence.provenance != alt_task::Provenance::Observed
            || evidence.original_report.is_none()
            || !self.local_collected_evidence_complete(&evidence)?
        {
            return Err(refused(
                "collector signing requires this repository's completed observed evidence",
            ));
        }
        let key = RecordKey {
            kind: alt_task_service::RecordKind::Evidence,
            id: id.into(),
        };
        let record = TaskRecord::Evidence(evidence);
        let (proof, _) = super::super::transfer_source::completed(
            self,
            &key,
            &record,
            task_limits().structured_bytes,
        )?;
        let path = self.root.join(key_file);
        if std::fs::symlink_metadata(&path)?.permissions().mode() & 0o077 != 0 {
            return Err(refused(
                "collector private key must be accessible only to its owner",
            ));
        }
        let seed: [u8; 32] = bounded(&path, 32)?
            .try_into()
            .map_err(|_| refused("collector key file must contain exactly 32 raw bytes"))?;
        let signer = alt_crypto::ed25519::SigningKey::from_bytes(&seed);
        let mut attestation = alt_task_service::CollectorAttestation {
            schema_version: 1,
            evidence: id.into(),
            source_proof: proof.id()?,
            signing_key_id: alt_hash::blake3::hash(signer.verifying_key().as_bytes())
                .to_hex()
                .to_string(),
            signature: String::new(),
        };
        attestation.signature =
            alt_input::hex(&signer.sign(&attestation.signing_bytes()?).to_bytes());
        emit(
            out,
            json_mode,
            json!({"attestation":attestation,"public_key":alt_input::hex(signer.verifying_key().as_bytes())}),
        )
    }
    pub(super) fn remote_review(
        &mut self,
        remote: &str,
        actor: &str,
        request: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task remote review")?;
        let binding = self.remote_binding(remote)?;
        let key = protocol::RequestKey {
            scope: binding.scope.clone(),
            actor_id: actor.into(),
            request_id: request.into(),
        };
        key.validate()?;
        let client = transport(&binding)?;
        check_connection(&client, &binding, true)?;
        let selection = client.review(&key)?;
        selection
            .manifest
            .validate_publication(&selection.publication, limits())?;
        if selection.publication.key != key {
            return Err(refused("ready review request mismatch"));
        }
        let digest = selection.publication.digest(limits())?;
        let parent = self.store.alt_dir.join("task-reviews");
        std::fs::create_dir_all(&parent)?;
        let directory = parent.join(&digest);
        let package = if directory.exists() {
            Package::open(&directory.join("sealed"), limits())?
        } else {
            let mut draft = alt_task_transfer::Draft::new(&parent, limits())?;
            for entry in &selection.manifest.entries {
                let mut file = alt_os::temp::tempfile_in(&parent)?;
                client.review_content(&key, &entry.blake3, entry.bytes, &mut file)?;
                file.rewind()?;
                draft.capture_selected(entry, &mut file)?;
            }
            draft.seal(selection.publication.clone(), &directory, task_limits())?
        };
        if package.request().publication.digest(limits())? != digest {
            return Err(refused("stored review selection mismatch"));
        }
        let received = Received::verify(&package, task_limits())?;
        let graph = received.verified().tasks.as_ref().expect("received tasks");
        let mut reviews = Vec::new();
        let mut display_bytes = limits().request_bytes as u64;
        for selected in &received.bundle().records {
            if let TaskRecord::Review(review) = graph.prepared().record(&selected.record)? {
                let changes = if review.changes.bytes <= display_bytes {
                    display_bytes -= review.changes.bytes;
                    let mut bytes = Vec::new();
                    package
                        .content(&review.changes.blake3)?
                        .take(review.changes.bytes.saturating_add(1))
                        .read_to_end(&mut bytes)?;
                    if bytes.len() as u64 != review.changes.bytes
                        || alt_hash::blake3::hash(&bytes).to_hex().as_str() != review.changes.blake3
                    {
                        return Err(refused(
                            "review changes no longer match their original content",
                        ));
                    }
                    Some(serde_json::from_slice::<serde_json::Value>(&bytes)?)
                } else {
                    None
                };
                reviews.push(json!({"review_id":selected.record.id,"review":review,"changes":changes,"original_changes_content":review.changes}));
            }
        }
        write_json(
            &directory.join("review.json"),
            &ReviewContext {
                binding,
                selection: selection.clone(),
            },
        )?;
        emit(
            out,
            json_mode,
            json!({"request_digest":digest,"selection":selection,"task_selection":received.bundle(),"reviews":reviews,"package":package.directory(),"state":"awaiting_explicit_approval"}),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn remote_approve(
        &mut self,
        remote: &str,
        actor: &str,
        request: &str,
        integration: &str,
        digest: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task remote approve")?;
        alt_task::check_id(integration)?;
        alt_task::check_id(digest)?;
        let binding = self.remote_binding(remote)?;
        let key = protocol::RequestKey {
            scope: binding.scope.clone(),
            actor_id: actor.into(),
            request_id: request.into(),
        };
        key.validate()?;
        let directory = self.store.alt_dir.join("task-reviews").join(digest);
        let previous: ReviewContext = json_file(&directory.join("review.json"))?;
        if serde_json::to_value(&previous.binding)? != serde_json::to_value(&binding)?
            || previous.selection.publication.key != key
            || previous.selection.publication.digest(limits())? != digest
        {
            return Err(refused(
                "approval must name the previously inspected origin and exact publication",
            ));
        }
        let package = Package::open(&directory.join("sealed"), limits())?;
        let received = Received::verify(&package, task_limits())?;
        if package.request().publication.digest(limits())? != digest
            || !received.bundle().records.iter().any(|r| {
                r.record.kind == alt_task_service::RecordKind::Integration
                    && r.record.id == integration
            })
        {
            return Err(refused("integration is not in the inspected selection"));
        }
        let client = transport(&binding)?;
        check_connection(&client, &binding, true)?;
        let current = client.review(&key)?;
        if current.publication.digest(limits())? != digest
            || current.current_refs != previous.selection.current_refs
            || current.policy_version != previous.selection.policy_version
        {
            return Err(refused(
                "review target or policy changed; inspect the current selection again",
            ));
        }
        client
            .approve(
                &key,
                integration,
                &previous
                    .selection
                    .approval_context(binding.transfer_limits)?,
            )
            .map_err(|e| remote_error(e, &key))?;
        emit(
            out,
            json_mode,
            json!({"request":key,"request_digest":digest,"integration_id":integration,"approved":true}),
        )
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_tests!("native-task-transfer-current_review", 0xc10a; ReviewContext);
}
