use super::{Binding, Budget, Res, connection, local::Journal, refused};
use alt_portable::{Envelope, IdentityMapping, ImportIntent, http::ImportState};
use alt_remote_protocol::{RequestKey, Scope};
use std::{io::Write, path::Path};

#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct ImportRequest {
    version: u32,
    binding: Binding,
    intent: ImportIntent,
    envelope: Envelope,
    archive: alt_portable::Blob,
}
#[derive(Clone)]
pub struct Source {
    pub scope: Scope,
    pub public_key: [u8; 32],
}
fn mappings(path: &Path, maximum: usize) -> Res<Vec<IdentityMapping>> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > maximum as u64 {
        return Err(refused(
            "identity mapping file exceeds its budget or is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take((maximum as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(refused(
            "identity mapping file changed or exceeds its budget",
        ));
    }
    let mut result: Vec<IdentityMapping> = alt_json::from_slice(&bytes)?;
    result
        .sort_by(|a, b| (&a.installation_id, &a.actor_id).cmp(&(&b.installation_id, &b.actor_id)));
    Ok(result)
}
/// Verify the complete local archive before reserving destination storage.
/// Staging is resumable; activation remains an explicit later operation.
pub struct Stage<'a> {
    pub repository: &'a str,
    pub archive: &'a Path,
    pub source: &'a Source,
    pub mappings: &'a Path,
}
pub fn stage(
    credential_file: &Path,
    input: Stage<'_>,
    journal_path: &Path,
    budget: Budget,
    out: &mut impl Write,
) -> Res<()> {
    let Stage {
        repository,
        archive,
        source,
        mappings: mapping_file,
    } = input;
    let journal = Journal::open(journal_path)?;
    journal.record("kind.json", &"import", 128)?;
    let connection = connection(credential_file, budget)?;
    let current = connection.client.repository(repository)?;
    let mappings = mappings(mapping_file, budget.metadata_bytes)?;
    let (envelope, archive) = journal.unpack(
        archive,
        &source.scope,
        &source.public_key,
        connection.package,
    )?;
    let request = if let Some(request) = journal.read::<ImportRequest>(
        "import.json",
        budget
            .metadata_bytes
            .checked_mul(2)
            .ok_or_else(|| refused("migration metadata budget overflow"))?,
    )? {
        if request.version != 1
            || request.binding != connection.binding
            || request.intent.key.scope != current.scope
            || request.intent.mappings != mappings
            || request.intent.source != source.scope
            || request.intent.source_public_key != crate::machine::hex(&source.public_key)
            || request.archive != archive
            || request.envelope.encode(connection.package)?
                != envelope.encode(connection.package)?
        {
            return Err(refused(
                "import journal binds different parameters or a previous recovery scope",
            ));
        }
        request
    } else {
        let intent = ImportIntent {
            version: 1,
            key: RequestKey {
                scope: current.scope,
                actor_id: connection.binding.actor.actor_id.clone(),
                request_id: crate::machine::hex(&crate::request::new_id()?),
            },
            target_public_key: connection.binding.receipt_public_key.clone(),
            source: source.scope.clone(),
            source_public_key: crate::machine::hex(&source.public_key),
            catalog_digest: envelope.catalog.digest(connection.package)?,
            mappings,
        };
        intent.validate_envelope(&envelope, connection.package)?;
        let request = ImportRequest {
            version: 1,
            binding: connection.binding.clone(),
            intent,
            envelope,
            archive,
        };
        journal.record(
            "import.json",
            &request,
            budget
                .metadata_bytes
                .checked_mul(2)
                .ok_or_else(|| refused("migration metadata budget overflow"))?,
        )?;
        request
    };
    request
        .intent
        .validate_envelope(&request.envelope, connection.package)?;
    let result = (|| -> Res<()> {
        let state = connection
            .client
            .portable_import_begin(&request.intent, &request.envelope, connection.package)
            .map_err(|error| super::uncertain(error, &request.intent.key, journal_path))?;
        match state {
            ImportState::Receiving { missing, .. } => {
                for blob in missing {
                    let index = request
                        .envelope
                        .catalog
                        .blobs
                        .binary_search_by(|b| b.blake3.cmp(&blob.blake3))
                        .map_err(|_| {
                            refused("destination requested content outside the signed archive")
                        })?;
                    if request.envelope.catalog.blobs[index] != blob {
                        return Err(refused("destination requested a different content length"));
                    }
                    let mut file = journal.content(&blob)?;
                    connection
                        .client
                        .portable_upload(&request.intent.key, &blob, &mut file)
                        .map_err(|error| {
                            super::uncertain(error, &request.intent.key, journal_path)
                        })?;
                }
            }
            ImportState::Committed { .. } => {}
            _ => {
                return Err(refused(
                    "original import is unavailable; query the same journal before creating a new intent",
                ));
            }
        }
        let state = connection
            .client
            .portable_import_status(&request.intent, connection.package)?;
        if let ImportState::Committed { receipt } = &state {
            journal.record("receipt.json", receipt, budget.metadata_bytes)?;
        }
        alt_json::to_writer(&mut *out, &state)?;
        out.write_all(b"\n")?;
        Ok(())
    })();
    result.map_err(|error| super::uncertain(error, &request.intent.key, journal_path))
}
#[derive(Clone, Copy)]
pub enum Command {
    Query,
    Activate,
    Abandon,
}
pub fn control(
    credential_file: &Path,
    journal_path: &Path,
    command: Command,
    budget: Budget,
    out: &mut impl Write,
) -> Res<()> {
    let journal = Journal::open(journal_path)?;
    let request: ImportRequest = journal
        .read(
            "import.json",
            budget
                .metadata_bytes
                .checked_mul(2)
                .ok_or_else(|| refused("migration metadata budget overflow"))?,
        )?
        .ok_or_else(|| refused("import request is missing from the journal"))?;
    let connection = connection(credential_file, budget)?;
    if request.version != 1 || request.binding != connection.binding {
        return Err(refused(
            "import journal belongs to another authenticated owner or instance",
        ));
    }
    request
        .intent
        .validate_envelope(&request.envelope, connection.package)?;
    let result = (|| -> Res<()> {
        let state = match command {
            Command::Query => connection
                .client
                .portable_import_status(&request.intent, connection.package)?,
            Command::Activate => connection
                .client
                .portable_import_activate(&request.intent, connection.package)
                .map_err(|error| super::uncertain(error, &request.intent.key, journal_path))?,
            Command::Abandon => connection
                .client
                .portable_import_abandon(&request.intent, connection.package)
                .map_err(|error| super::uncertain(error, &request.intent.key, journal_path))?,
        };
        if let ImportState::Committed { receipt } = &state {
            journal.record("receipt.json", receipt, budget.metadata_bytes)?;
        }
        alt_json::to_writer(&mut *out, &state)?;
        out.write_all(b"\n")?;
        Ok(())
    })();
    result.map_err(|error| {
        if matches!(command, Command::Query) {
            error
        } else {
            super::uncertain(error, &request.intent.key, journal_path)
        }
    })
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_fixture_tests!("portable-import"; ImportRequest);
}
