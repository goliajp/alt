//! What is done with one request for publication: its content taken in, the
//! request looked at, prepared, published and approved.

use super::*;

pub(super) fn execute(
    store: &mut Store,
    credential: &Credential,
    options: &NativeOptions,
    key: alt_remote_protocol::RequestKey,
    action: RequestAction,
    source: &mut dyn Read,
) -> Result<Output> {
    match action {
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
    }
}
