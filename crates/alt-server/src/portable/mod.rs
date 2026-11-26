mod download;
pub(crate) use download::turned;
pub mod route;
use self::download::Asked;
pub(crate) use self::download::Download;
use self::route::{ExportAction, ImportAction, PortableRoute};
use crate::{NativeOptions, Output};
use alt_portable::{
    ImportIntent,
    http::{BeginImport, Bounds, Capabilities, ImportState, InspectArchive},
};
use alt_server_store::{Credential, Error, Result, Store};
use std::io::Read;

/// the media type of an archive
pub(crate) const ARCHIVE: &str = "application/vnd.alt.portable-v1";

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

/// A portable request once what it needs of the database is read: its answer,
/// the download that is written without the database, or the share of the room
/// of a download put off to wait for its turn, with which it is asked again.
pub(crate) enum Step {
    Done(Output),
    Download(Box<Download>),
    Turn(alt_shares::Share),
}

pub(crate) fn execute(
    store: &mut Store,
    credential: &Credential,
    options: &NativeOptions,
    route: PortableRoute,
    source: &mut dyn Read,
    refresh: &mut dyn FnMut(&mut Store) -> Result<Credential>,
    // of the room responses have on disk, for what is written out to be sent
    sending: Option<alt_shares::Share>,
) -> Result<Step> {
    use crate::native::{body, empty, json};
    let settings = bounds(options)?;
    let limits = settings.verification(store.limits(), options.task_limits)?;
    let done = match route {
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
                ExportAction::Archive | ExportAction::Content(_) => {
                    let export =
                        store.open_portable_export(credential, &scope, &id, limits.package)?;
                    let asked = match action {
                        ExportAction::Content(digest) => Asked::Content(digest),
                        _ => Asked::Archive,
                    };
                    // a large one has its turn before it is written, or is put off
                    // to wait for it with nothing of the export held
                    let mut sending = sending;
                    if let Some(share) = sending.as_mut()
                        && share.expect(download::bytes(&export, &asked)).is_err()
                    {
                        return match share.put_off() {
                            true => Ok(Step::Turn(sending.expect("a share"))),
                            false => Err(Error::Busy("responses to fetches")),
                        };
                    }
                    let download = Download::new(export, asked, limits.package, sending);
                    return Ok(Step::Download(Box::new(download)));
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
                json(&alt_json::json!({"blake3":digest,"verified":true}))
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
                    return json(&ImportState::Committed { receipt }).map(Step::Done);
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
    };
    done.map(Step::Done)
}
