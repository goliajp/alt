use super::*;
use alt_task_service::{
    ArtifactSource, RecordKey, RecordKind, SourceCompletion, SourceRecovery, TaskRecord,
};
use alt_task_transfer::{Error, Provenance, Result, Source};
use std::io::{Cursor, Read};

pub(super) struct LocalSource<'a, 'repo> {
    pub repo: &'a NativeRepo<'repo>,
    pub tasks: &'a alt_task::Store,
    pub received: &'a [alt_task_transfer::Received<'a>],
}
pub(super) fn bounded(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > maximum as u64 {
        return Err(Error::Limit("local structured file"));
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(Error::Limit("local structured file"));
    }
    Ok(bytes)
}
fn artifact(bytes: &[u8]) -> Artifact {
    Artifact {
        blake3: blake3::hash(bytes).to_hex().to_string(),
        bytes: bytes.len() as u64,
    }
}
pub(super) struct ProofBytes(pub Vec<Vec<u8>>);
impl ArtifactSource for ProofBytes {
    fn open(&self, artifact: &Artifact) -> alt_task_service::Result<Box<dyn Read + '_>> {
        let bytes = self
            .0
            .iter()
            .find(|bytes| {
                bytes.len() as u64 == artifact.bytes
                    && blake3::hash(bytes).to_hex().as_str() == artifact.blake3
            })
            .ok_or(alt_task_service::Error::Integrity("local proof artifact"))?;
        Ok(Box::new(Cursor::new(bytes)))
    }
}

/// The exact original request files; absence never becomes a synthetic
/// completion. Used for both publishing and record-bound local checks.
pub(super) fn completed(
    repo: &NativeRepo<'_>,
    key: &RecordKey,
    record: &TaskRecord,
    maximum: usize,
) -> Result<(SourceCompletion, ProofBytes)> {
    let request = record
        .creator_request()
        .ok_or(Error::Invalid("record has no creator request"))?;
    crate::request::parse_id(request).map_err(|_| Error::Invalid("creator request identity"))?;
    let directory = repo.store.alt_dir.join("requests").join(request);
    let intent_raw = bounded(&directory.join("intent.json"), maximum)?;
    let completion_raw = bounded(&directory.join("completed.json"), maximum)?;
    let response_raw = bounded(&directory.join("response.bin"), maximum)?;
    let intent: alt_command_protocol::Intent = serde_json::from_slice(&intent_raw)?;
    let completion: alt_command_protocol::Completion = serde_json::from_slice(&completion_raw)?;
    let mut proof = SourceCompletion {
        schema_version: 1,
        record: key.clone(),
        origin: Scope {
            repository: intent.repository,
            workspace: intent.workspace,
        },
        request_id: request.into(),
        intent: artifact(&intent_raw),
        completion: artifact(&completion_raw),
        response: artifact(&response_raw),
        ref_operation: None,
    };
    let mut raw = ProofBytes(vec![intent_raw, completion_raw, response_raw]);
    let checked = alt_task_service::check_source(record, &proof, &raw, maximum)?;
    if let TaskRecord::Integration(integration) = record {
        let mut matched = None;
        for op in repo.store.refs.oplog().ops() {
            if !completion.operations.contains(&op.id.to_string()) {
                continue;
            }
            let body = op
                .body()
                .map_err(|_| Error::Invalid("local operation bytes"))?;
            if body.len() > maximum {
                return Err(Error::Limit("source operation bytes"));
            }
            let candidate_artifact = artifact(&body);
            let source = ProofBytes(vec![body.clone()]);
            let mut candidate = proof.clone();
            candidate.ref_operation = Some(candidate_artifact.clone());
            if alt_task_service::verify_integration_operation(
                integration,
                &candidate,
                &checked.original_operations,
                &source,
                maximum,
            )
            .is_ok()
            {
                if matched.is_some() {
                    return Err(Error::Invalid("ambiguous source integration operation"));
                }
                matched = Some((candidate_artifact, body));
            }
        }
        let (reference, body) =
            matched.ok_or(Error::Invalid("source integration operation missing"))?;
        proof.ref_operation = Some(reference);
        raw.0.push(body);
    }
    Ok((proof, raw))
}
impl ArtifactSource for LocalSource<'_, '_> {
    fn open(&self, artifact: &Artifact) -> alt_task_service::Result<Box<dyn Read + '_>> {
        Ok(Box::new(Cursor::new(self.tasks.blob(artifact)?)))
    }
}
impl alt_repository_service::ObjectSource for LocalSource<'_, '_> {
    fn open(
        &self,
        oid: ObjectId,
    ) -> alt_repository_service::Result<Option<alt_repository_service::ObjectReader<'_>>> {
        let object =
            self.repo.store.odb.get(&oid).map_err(|_| {
                alt_repository_service::Error::Integrity("local object unavailable")
            })?;
        Ok(object.map(|object| alt_repository_service::ObjectReader {
            kind: object.kind,
            bytes: object.data.len() as u64,
            reader: Box::new(Cursor::new(object.data)),
        }))
    }
}
impl Source for LocalSource<'_, '_> {
    fn record(&self, key: &RecordKey, maximum: usize) -> Result<Vec<u8>> {
        key.validate()?;
        for received in self.received {
            if received.bundle().records.iter().any(|r| &r.record == key) {
                return received.record(key, maximum);
            }
        }
        let path = self
            .repo
            .store
            .alt_dir
            .join("task-data/records")
            .join(key.kind.as_str())
            .join(format!("{}.json", key.id));
        let raw = bounded(&path, maximum)?;
        TaskRecord::decode(key, &raw, maximum)?;
        Ok(raw)
    }
    fn provenance(
        &self,
        key: &RecordKey,
        record: &TaskRecord,
        maximum: usize,
    ) -> Result<Provenance> {
        for received in self.received {
            if received.bundle().records.iter().any(|r| &r.record == key) {
                return received.provenance(key, record, maximum);
            }
        }
        match completed(self.repo, key, record, maximum) {
            Ok((proof, raw)) => return Ok(Provenance::Completed { proof, raw: raw.0 }),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        // A lost original response can only be represented by an explicitly
        // retained, separately completed collection receipt.
        let mut receipts = Vec::new();
        match record {
            TaskRecord::Integration(_) => {
                for (id, receipt) in self.tasks.list::<alt_task::IntegrationReceipt>()? {
                    if receipt.integration == key.id {
                        receipts.push((
                            RecordKey {
                                kind: RecordKind::IntegrationReceipt,
                                id,
                            },
                            Some(receipt.ref_operation),
                        ));
                    }
                }
            }
            TaskRecord::Delivery(_) => {
                for (id, receipt) in self.tasks.list::<alt_task::DeliveryReceipt>()? {
                    if receipt.delivery == key.id {
                        receipts.push((
                            RecordKey {
                                kind: RecordKind::DeliveryReceipt,
                                id,
                            },
                            None,
                        ));
                    }
                }
            }
            _ => return Err(Error::Invalid("source completion missing")),
        }
        if receipts.len() != 1 {
            return Err(Error::Invalid("missing or ambiguous recovery receipt"));
        }
        let (receipt, operation) = receipts.pop().expect("one receipt");
        let request = record
            .creator_request()
            .ok_or(Error::Invalid("recovery request"))?;
        crate::request::parse_id(request)
            .map_err(|_| Error::Invalid("recovery request identity"))?;
        let raw_intent = bounded(
            &self
                .repo
                .store
                .alt_dir
                .join("requests")
                .join(request)
                .join("intent.json"),
            maximum,
        )?;
        let intent: alt_command_protocol::Intent = serde_json::from_slice(&raw_intent)?;
        let mut proof = SourceRecovery {
            schema_version: 1,
            record: key.clone(),
            origin: Scope {
                repository: intent.repository,
                workspace: intent.workspace,
            },
            request_id: request.into(),
            intent: artifact(&raw_intent),
            receipt,
            ref_operation: None,
        };
        let mut raw = vec![raw_intent];
        if let Some(id) = operation {
            let op = self
                .repo
                .store
                .refs
                .oplog()
                .ops()
                .iter()
                .find(|op| op.id.to_string() == id)
                .ok_or(Error::Invalid("recovered operation missing"))?;
            let body = op
                .body()
                .map_err(|_| Error::Invalid("recovered operation bytes"))?;
            if body.len() > maximum {
                return Err(Error::Limit("recovered operation bytes"));
            }
            proof.ref_operation = Some(artifact(&body));
            raw.push(body);
        }
        Ok(Provenance::Recovered { proof, raw })
    }
}
