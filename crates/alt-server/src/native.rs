use crate::{RequestAction, Route, ValidatedWork};
use alt_remote_protocol::{Capabilities, Feature, ObjectFormat, RequestObservation, RequestState};
use alt_server_store::{Credential, Error, Result, Store};
use alt_task_service::TaskLimits;
use serde::{Deserialize, Serialize};
use std::io::Read;

pub use alt_remote_protocol::{ApprovalRequest, BeginRequest};
#[derive(Clone)]
pub struct NativeOptions {
    pub task_limits: TaskLimits,
    pub upload_ttl_ms: u64,
    pub worker_lease_ms: u64,
    pub approval_ttl_ms: u64,
}
pub enum Output {
    Json(serde_json::Value),
    GitReport {
        bytes: Vec<u8>,
        request: alt_remote_protocol::RequestKey,
    },
    Bytes {
        content_type: &'static str,
        bytes: Vec<u8>,
    },
    Temporary {
        content_type: &'static str,
        bytes: u64,
        file: tempfile::NamedTempFile,
    },
    File {
        bytes: u64,
        digest: String,
        file: std::fs::File,
    },
}
fn json(value: &impl Serialize) -> Result<Output> {
    Ok(Output::Json(serde_json::to_value(value)?))
}
fn body<T: for<'de> Deserialize<'de>>(source: &mut dyn Read, max: usize) -> Result<T> {
    let mut raw = Vec::new();
    source
        .take((max as u64).saturating_add(1))
        .read_to_end(&mut raw)?;
    if raw.len() > max {
        return Err(Error::Limit("request body"));
    }
    Ok(serde_json::from_slice(&raw)?)
}
fn empty(source: &mut dyn Read) -> Result<()> {
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
        Route::GitAdvertise(..) | Route::GitUpload(..) | Route::GitReceive(..) => {
            Err(Error::Precondition("Git route requires Git dispatch"))
        }
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
                &serde_json::json!({"capabilities":caps,"receipt_public_key":store.receipt_public_key()}),
            )
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
                json(&serde_json::json!({"blake3":digest,"verified":true}))
            }
            RequestAction::Assemble(digest) => {
                empty(source)?;
                store.assemble_content(credential, &key, &digest)?;
                json(&serde_json::json!({"blake3":digest,"verified":true}))
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
                json(&serde_json::json!({"blake3":digest,"verified":true}))
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
                    &serde_json::json!({"request":key,"integration_id":request.integration_id,"approved":true}),
                )
            }
        },
    }
}
