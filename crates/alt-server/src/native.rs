use crate::{RequestAction, Route, ValidatedWork};
use alt_remote_protocol::{Capabilities, Feature, ObjectFormat, RequestObservation, RequestState};
use alt_server_store::{Credential, Error, Result, Store};
use alt_task_service::TaskLimits;
use serde::Serialize;
use std::io::Read;

pub use alt_remote_protocol::{ApprovalRequest, BeginRequest};
#[derive(Clone)]
pub struct NativeOptions {
    pub portable: Option<alt_portable::http::Bounds>,
    pub task_limits: TaskLimits,
    pub upload_ttl_ms: u64,
    pub worker_lease_ms: u64,
    pub approval_ttl_ms: u64,
}
pub enum Output {
    Json(alt_json::Value),
    GitReport {
        bytes: Vec<u8>,
        request: alt_remote_protocol::RequestKey,
    },
    Bytes {
        content_type: &'static str,
        bytes: Vec<u8>,
    },
    /// a whole repository as one Git pack: the stretches of stored bytes it consists
    /// of, and whether the pack checksum has to be computed over them and sent last
    WholePack(Vec<alt_pack_segment::Piece>, bool),
    Temporary {
        content_type: &'static str,
        bytes: u64,
        file: alt_os::temp::NamedTempFile,
    },
    File {
        bytes: u64,
        digest: String,
        file: alt_server_store::StoredReader,
    },
}
pub(crate) fn json(value: &impl alt_json::ToJson) -> Result<Output> {
    Ok(Output::Json(alt_json::to_value(value)?))
}
pub(crate) fn body<T: alt_json::FromJson>(source: &mut dyn Read, max: usize) -> Result<T> {
    let mut raw = Vec::new();
    source
        .take((max as u64).saturating_add(1))
        .read_to_end(&mut raw)?;
    if raw.len() > max {
        return Err(Error::Limit("request body"));
    }
    Ok(alt_json::from_slice(&raw)?)
}
/// For the vendored contract types, which have serde implementations only.
pub(crate) fn json_serde(value: &impl Serialize) -> Result<Output> {
    Ok(Output::Json(alt_json_serde::to_value(value)?))
}
pub(crate) fn body_serde<T: serde::de::DeserializeOwned>(
    source: &mut dyn Read,
    max: usize,
) -> Result<T> {
    Ok(body::<alt_json_serde::Serde<T>>(source, max)?.0)
}
pub(crate) fn empty(source: &mut dyn Read) -> Result<()> {
    if source.read(&mut [0u8; 1])? != 0 {
        return Err(Error::Precondition("unexpected request body"));
    }
    Ok(())
}
pub fn execute_native(
    store: &mut Store,
    credential: &Credential,
    options: &NativeOptions,
    route: Route,
    source: &mut dyn Read,
) -> Result<Output> {
    match route {
        Route::ChangeLink(_) => Err(Error::Precondition(
            "change-link route requires refreshed authority dispatch",
        )),
        Route::Portable(_) => Err(Error::Precondition(
            "portable route requires refreshed authority dispatch",
        )),
        Route::GitAdvertise(..) | Route::GitUpload(..) | Route::GitReceive(..) => {
            Err(Error::Precondition("Git route requires Git dispatch"))
        }
        Route::Metrics => Err(Error::Precondition("metrics require process dispatch")),
        Route::Health => Err(Error::Precondition(
            "health handled before authenticated dispatch",
        )),
        Route::Capabilities => {
            empty(source)?;
            store.principal(credential)?;
            let caps = Capabilities {
                protocol_min: 1,
                protocol_max: 1,
                installation_id: store.spec().installation_id.clone(),
                runtime_generation: store.spec().runtime_generation,
                deployment_mode: store.spec().deployment_mode,
                object_formats: vec![ObjectFormat::Sha1],
                features: vec![
                    Feature::GitSmartHttp,
                    Feature::Publications,
                    Feature::TaskRecords,
                    Feature::TaskBundlesV1,
                    Feature::Receipts,
                ],
                limits: store.limits(),
            };
            json(
                &alt_json::json!({"capabilities":caps,"receipt_public_key":store.receipt_public_key()}),
            )
        }
        Route::IssueAgent => {
            let request: alt_remote_protocol::delegation::DelegationRequest = body(source, 16_384)?;
            json(&store.issue_agent(credential, &request)?)
        }
        Route::Delegations(after) => {
            empty(source)?;
            json(&store.agent_delegations(credential, after.as_deref().unwrap_or(""))?)
        }
        Route::Delegation(digest) => {
            empty(source)?;
            json(&store.agent_delegation(credential, &digest)?)
        }
        Route::RevokeAgent(digest) => {
            empty(source)?;
            store.revoke_agent(credential, &digest)?;
            json(&alt_json::json!({"revoked":true,"credential_id":digest}))
        }
        Route::Principal => {
            empty(source)?;
            json(&store.principal(credential)?)
        }
        Route::Repository(repository) => {
            empty(source)?;
            json(&store.repository_info(credential, &repository)?)
        }
        Route::Begin(scope) => {
            let request: BeginRequest = body(source, store.limits().request_bytes)?;
            if request.publication.key.scope != scope {
                return Err(Error::Unauthorized);
            }
            json(&store.begin(
                credential,
                &request.publication,
                &request.manifest,
                options.upload_ttl_ms,
            )?)
        }
        Route::Content(scope, digest) => {
            empty(source)?;
            let (bytes, file) = store.open_published_content(credential, &scope, &digest)?;
            Ok(Output::File {
                bytes,
                digest,
                file,
            })
        }
        Route::Bundles(scope, root, after) => {
            empty(source)?;
            json(&store.bundles_for_record(credential, &scope, &root, after.as_deref(), 20)?)
        }
        Route::Records(scope, kind, after) => {
            empty(source)?;
            json(&store.published_records(credential, &scope, kind, after.as_deref(), 100)?)
        }
        Route::Request(key, action) => match action {
            RequestAction::UploadStatus => {
                empty(source)?;
                json(&store.upload_status(credential, &key)?)
            }
            RequestAction::UploadChunk(digest) => {
                store.upload_chunk(credential, &key, &digest, source)?;
                json(&alt_json::json!({"blake3":digest,"verified":true}))
            }
            RequestAction::Assemble(digest) => {
                empty(source)?;
                store.assemble_content(credential, &key, &digest)?;
                json(&alt_json::json!({"blake3":digest,"verified":true}))
            }
            RequestAction::Review => {
                empty(source)?;
                json(&store.review_selection(credential, &key)?)
            }
            RequestAction::ReviewContent(digest) => {
                empty(source)?;
                let (bytes, file) = store.open_review_content(credential, &key, &digest)?;
                Ok(Output::File {
                    bytes,
                    digest,
                    file,
                })
            }
            RequestAction::Query => {
                empty(source)?;
                json(&store.query(credential, &key)?)
            }
            RequestAction::Upload(digest) => {
                let entries = store.selected_upload(credential, &key, &digest)?;
                let entry = &entries[0];
                let proof = store.bytes().put(
                    &key.scope,
                    &digest,
                    entry.bytes,
                    store.limits().object_bytes,
                    source,
                )?;
                for entry in entries {
                    store.register_content(credential, &key, &entry, &proof)?;
                }
                json(&alt_json::json!({"blake3":digest,"verified":true}))
            }
            RequestAction::Prepare | RequestAction::Publish => {
                empty(source)?;
                let observation = store.query(credential, &key)?;
                match &observation {
                    RequestObservation::Recorded { result, .. }
                        if matches!(
                            **result,
                            RequestState::Committed { .. }
                                | RequestState::Rejected { .. }
                                | RequestState::Expired { .. }
                        ) =>
                    {
                        return json(&observation);
                    }
                    RequestObservation::Unknown { .. } => return json(&observation),
                    _ => {}
                }
                let limits = store.limits();
                let work = store.acquire_publication(
                    credential,
                    &key,
                    options.worker_lease_ms,
                    limits.graph_objects,
                )?;
                // Validation failures leave the immutable request available for
                // inspection. The fenced lease expires even after process loss.
                let validated = ValidatedWork::verify(work, limits, options.task_limits)?;
                let result = (|| {
                    validated.mark_ready(store, credential)?;
                    if action == RequestAction::Publish {
                        let receipt = validated.publish(store, credential)?;
                        return json(&RequestObservation::Recorded {
                            key: key.clone(),
                            result: Box::new(RequestState::Committed {
                                receipt: Box::new(receipt),
                            }),
                        });
                    }
                    json(&store.query(credential, &key)?)
                })();
                if action == RequestAction::Prepare || result.is_err() {
                    let release = store.release_publication(credential, validated.work());
                    if result.is_ok() {
                        release?;
                    }
                }
                result
            }
            RequestAction::Approve => {
                let request: ApprovalRequest = body(source, store.limits().request_bytes)?;
                alt_task::check_id(&request.integration_id)
                    .map_err(|_| Error::Precondition("integration id"))?;
                let limits = store.limits();
                let work = store.acquire_review(
                    credential,
                    &key,
                    options.worker_lease_ms,
                    limits.graph_objects,
                )?;
                let validated = ValidatedWork::verify(work, limits, options.task_limits)?;
                let result = validated.approve(
                    store,
                    credential,
                    &request.integration_id,
                    &request.inspected,
                    options.approval_ttl_ms,
                );
                let _ = store.release_publication(credential, validated.work());
                result?;
                json(
                    &alt_json::json!({"request":key,"integration_id":request.integration_id,"approved":true}),
                )
            }
        },
    }
}
