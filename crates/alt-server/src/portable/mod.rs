pub mod route;
use self::route::{ExportAction, ImportAction, PortableRoute};
use crate::{NativeOptions, Output};
use alt_portable::{
    BlobSource, ImportIntent,
    http::{BeginImport, Bounds, Capabilities, ImportState, InspectArchive},
};
use alt_server_store::{Credential, Error, Result, Store};
use std::io::{Read, Seek, SeekFrom, Write};

fn bounds(options: &NativeOptions) -> Result<Bounds> {
    options
        .portable
        .ok_or(Error::Precondition("portable transfer is not configured"))
}

/// Authentication and current selection checks run before spooling a body.
/// The actual operation repeats these checks after the body arrives.
pub fn request_bytes(
    store: &mut Store,
    credential: &Credential,
    options: &NativeOptions,
    route: &PortableRoute,
) -> Result<u64> {
    let limits = bounds(options)?;
    match route {
        PortableRoute::Inspect(scope) => {
            store.check_portable_inspection(credential, scope)?;
            (limits.metadata_bytes as u64)
                .checked_mul(2)
                .and_then(|v| v.checked_add(1024))
                .ok_or(Error::Limit("portable request bytes"))
        }
        PortableRoute::Import(key, ImportAction::Upload(digest)) => Ok(store
            .selected_portable_upload(credential, key, digest)?
            .bytes),
        PortableRoute::Import(key, ImportAction::Begin | ImportAction::Activate) => {
            let info = store.repository_info(credential, &key.scope.repository_instance_id)?;
            let principal = store.principal(credential)?;
            if info.scope != key.scope || principal.actor_id != key.actor_id {
                return Err(Error::Unauthorized);
            }
            // Begin carries both the catalog and the identity mapping intent.
            (limits.metadata_bytes as u64)
                .checked_mul(2)
                .and_then(|v| v.checked_add(1024))
                .ok_or(Error::Limit("portable request bytes"))
        }
        _ => {
            store.principal(credential)?;
            Ok(0)
        }
    }
}

pub fn execute(
    store: &mut Store,
    credential: &Credential,
    options: &NativeOptions,
    route: PortableRoute,
    source: &mut dyn Read,
    refresh: &mut dyn FnMut(&mut Store) -> Result<Credential>,
) -> Result<Output> {
    use crate::native::{body, empty, json};
    let settings = bounds(options)?;
    let limits = settings.verification(store.limits(), options.task_limits)?;
    match route {
        PortableRoute::Inspect(scope) => {
            store.check_portable_inspection(credential, &scope)?;
            let maximum = settings
                .metadata_bytes
                .checked_mul(2)
                .and_then(|v| v.checked_add(1024))
                .ok_or(Error::Limit("portable request bytes"))?;
            let input: InspectArchive = body(source, maximum)?;
            json(&input.verify(limits.package)?)
        }
        PortableRoute::Capabilities => {
            empty(source)?;
            store.principal(credential)?;
            json(&Capabilities {
                version: 1,
                installation_id: store.spec().installation_id.clone(),
                receipt_public_key: store.receipt_public_key(),
                bounds: settings,
            })
        }
        PortableRoute::Export(scope, id, action) => {
            empty(source)?;
            match action {
                ExportAction::Prepare => {
                    let work = store.verify_portable_export(credential, &scope, &id, limits)?;
                    let current = refresh(store)?;
                    store.complete_portable_export(&current, work)?;
                    json(&store.query_portable_export(credential, &scope, &id, limits.package)?)
                }
                ExportAction::Query => {
                    json(&store.query_portable_export(credential, &scope, &id, limits.package)?)
                }
                ExportAction::Freeze => {
                    store.freeze_portable_export(credential, &scope, &id, limits.package)?;
                    json(&store.query_portable_export(credential, &scope, &id, limits.package)?)
                }
                ExportAction::Reopen => {
                    store.release_portable_freeze(credential, &scope, &id)?;
                    json(&store.query_portable_export(credential, &scope, &id, limits.package)?)
                }
                ExportAction::Retire => {
                    store.retire_portable_export(credential, &scope, &id)?;
                    json(&store.query_portable_export(credential, &scope, &id, limits.package)?)
                }
                ExportAction::Archive => {
                    let export =
                        store.open_portable_export(credential, &scope, &id, limits.package)?;
                    let mut file = tempfile::NamedTempFile::new()?;
                    alt_portable::write_archive(
                        file.as_file_mut(),
                        export.envelope(),
                        &export,
                        limits.package,
                    )?;
                    let bytes = file.as_file().metadata()?.len();
                    file.as_file_mut().seek(SeekFrom::Start(0))?;
                    Ok(Output::Temporary {
                        content_type: "application/vnd.alt.portable-v1",
                        bytes,
                        file,
                    })
                }
                ExportAction::Content(digest) => {
                    let export =
                        store.open_portable_export(credential, &scope, &id, limits.package)?;
                    let blob = export
                        .envelope()
                        .catalog
                        .blobs
                        .iter()
                        .find(|b| b.blake3 == digest)
                        .ok_or(Error::Unauthorized)?;
                    let mut input = export.open(blob)?;
                    let mut file = tempfile::NamedTempFile::new()?;
                    let mut hash = blake3::Hasher::new();
                    let mut remaining = blob.bytes;
                    let mut buffer = [0u8; 65536];
                    while remaining > 0 {
                        let count = input.read(&mut buffer[..remaining.min(65536) as usize])?;
                        if count == 0 {
                            return Err(Error::Integrity("portable export truncated"));
                        }
                        hash.update(&buffer[..count]);
                        file.write_all(&buffer[..count])?;
                        remaining -= count as u64;
                    }
                    if input.read(&mut [0])? != 0
                        || hash.finalize().to_hex().as_str() != blob.blake3
                    {
                        return Err(Error::Integrity("portable export bytes changed"));
                    }
                    file.as_file_mut().seek(SeekFrom::Start(0))?;
                    Ok(Output::File {
                        bytes: blob.bytes,
                        digest,
                        file: alt_server_store::StoredReader::from_file(
                            file.into_file(),
                            blob.bytes,
                        )?,
                    })
                }
            }
        }
        PortableRoute::Import(key, action) => match action {
            ImportAction::Begin => {
                let maximum = settings
                    .metadata_bytes
                    .checked_mul(2)
                    .and_then(|v| v.checked_add(1024))
                    .ok_or(Error::Limit("portable request bytes"))?;
                let request: BeginImport = body(source, maximum)?;
                if request.intent.key != key {
                    return Err(Error::Unauthorized);
                }
                json(&store.begin_portable_import(
                    credential,
                    &request.intent,
                    &request.envelope,
                    settings.import_ttl_ms,
                    limits.package,
                )?)
            }
            ImportAction::Query => {
                empty(source)?;
                json(&store.query_portable_import(credential, &key)?)
            }
            ImportAction::Upload(digest) => {
                let blob = store.selected_portable_upload(credential, &key, &digest)?;
                let proof = store.bytes().put(
                    &key.scope,
                    &digest,
                    blob.bytes,
                    limits.package.blob_bytes,
                    source,
                )?;
                store.register_portable_content(credential, &key, &proof)?;
                json(&serde_json::json!({"blake3":digest,"verified":true}))
            }
            ImportAction::Activate => {
                let intent: ImportIntent = body(source, settings.metadata_bytes)?;
                if intent.key != key {
                    return Err(Error::Unauthorized);
                }
                if let ImportState::Committed { receipt } =
                    store.query_portable_import(credential, &key)?
                {
                    if receipt.body.intent != intent {
                        return Err(Error::Precondition("portable import intent changed"));
                    }
                    return json(&ImportState::Committed { receipt });
                }
                let work = store.verify_portable_import(credential, &intent, limits)?;
                let current = refresh(store)?;
                let receipt = store.activate_portable_import(&current, &work)?;
                json(&ImportState::Committed {
                    receipt: Box::new(receipt),
                })
            }
            ImportAction::Abandon => {
                empty(source)?;
                store.abandon_portable_import(credential, &key)?;
                json(&store.query_portable_import(credential, &key)?)
            }
        },
    }
}
