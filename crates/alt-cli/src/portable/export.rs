use super::{Binding, Budget, Res, connection, local::Journal, refused};
use alt_portable::{Envelope, http::ExportState};
use alt_remote_protocol::RequestKey;
use alt_wire_http::native::portable::ExportCommand;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct ExportRequest {
    version: u32,
    binding: Binding,
    key: RequestKey,
    output: PathBuf,
}
#[derive(Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct ArchiveFact {
    version: u32,
    catalog_digest: String,
    archive: alt_portable::Blob,
    output: PathBuf,
}
fn output_path(path: &Path) -> Res<PathBuf> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| refused("archive output requires a file name"))?;
    Ok(std::fs::canonicalize(parent)?.join(name))
}
fn file_digest(file: &mut File) -> Res<alt_portable::Blob> {
    file.rewind()?;
    let bytes = file.metadata()?.len();
    let mut remaining = bytes;
    let mut hash = alt_hash::blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let count = file.read(&mut buffer[..remaining.min(65536) as usize])?;
        if count == 0 {
            return Err(refused("archive changed while reading"));
        }
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if file.read(&mut [0])? != 0 {
        return Err(refused("archive changed while reading"));
    }
    Ok(alt_portable::Blob {
        blake3: hash.finalize().to_hex().to_string(),
        bytes,
    })
}

pub fn export(
    credential_file: &Path,
    repository: &str,
    output: &Path,
    journal_path: &Path,
    budget: Budget,
    out: &mut impl Write,
) -> Res<()> {
    let journal = Journal::open(journal_path)?;
    journal.record("kind.json", &"export", 128)?;
    let connection = connection(credential_file, budget)?;
    let current = connection.client.repository(repository)?;
    let output = output_path(output)?;
    let request = if let Some(request) =
        journal.read::<ExportRequest>("export.json", budget.metadata_bytes)?
    {
        if request.version != 1
            || request.binding != connection.binding
            || request.output != output
            || request.key.scope != current.scope
        {
            return Err(refused(
                "export journal belongs to different parameters or a previous recovery scope",
            ));
        }
        request
    } else {
        if output.try_exists()? {
            return Err(refused(
                "archive output already exists; choose an unused destination",
            ));
        }
        let request = ExportRequest {
            version: 1,
            binding: connection.binding.clone(),
            key: RequestKey {
                scope: current.scope,
                actor_id: connection.binding.actor.actor_id.clone(),
                request_id: crate::machine::hex(&crate::request::new_id()?),
            },
            output: output.clone(),
        };
        journal.record("export.json", &request, budget.metadata_bytes)?;
        request
    };
    // The request is durable before any mutation. Repeating it captures no
    // newer source state and can only return the same immutable export.
    let result = (|| -> Res<()> {
        let state = connection
            .client
            .portable_export(
                &request.key,
                ExportCommand::Prepare,
                &connection.public_key,
                connection.package,
            )
            .map_err(|error| super::uncertain(error, &request.key, journal_path))?;
        let ExportState::Available { envelope, frozen } = state else {
            return Err(refused(
                "original export is unavailable; inspect its state before starting another snapshot",
            ));
        };
        journal.record("envelope.json", &envelope, budget.metadata_bytes)?;
        for blob in &envelope.catalog.blobs {
            journal.receive(blob, |file| {
                connection.client.portable_download(
                    &request.key.scope,
                    &request.key.request_id,
                    blob,
                    file,
                )?;
                Ok(())
            })?;
        }
        let parent = output
            .parent()
            .ok_or_else(|| refused("archive output parent"))?;
        let mut temporary = alt_os::temp::NamedTempFile::new_in(parent)?;
        alt_portable::write_archive(
            temporary.as_file_mut(),
            &envelope,
            &journal,
            connection.package,
        )?;
        temporary.as_file().sync_all()?;
        let archive = file_digest(temporary.as_file_mut())?;
        match std::fs::symlink_metadata(&output) {
            Ok(metadata) => {
                if !metadata.is_file() {
                    return Err(refused("archive output is not a regular file"));
                }
                use std::os::unix::fs::OpenOptionsExt;
                let mut existing = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
                    .open(&output)?;
                if existing.metadata()?.len() != archive.bytes
                    || file_digest(&mut existing)? != archive
                {
                    return Err(refused("archive output contains different bytes"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                temporary.persist_noclobber(&output)?;
            }
            Err(error) => return Err(error.into()),
        }
        File::open(parent)?.sync_all()?;
        let fact = ArchiveFact {
            version: 1,
            catalog_digest: envelope.catalog.digest(connection.package)?,
            archive,
            output,
        };
        journal.record("archive.json", &fact, budget.metadata_bytes)?;
        serde_json::to_writer(
            &mut *out,
            &serde_json::json!({"schema_version":1,"state":"exported","source":request.key.scope,"source_public_key":envelope.catalog.source_public_key,"export_id":request.key.request_id,"snapshot_sequence":envelope.catalog.snapshot_sequence.to_string(),"source_frozen":frozen,"archive":fact}),
        )?;
        out.write_all(b"\n")?;
        Ok(())
    })();
    result.map_err(|error| super::uncertain(error, &request.key, journal_path))
}
/// Explicit source control uses the same snapshot identity. No source freeze,
/// retirement or endpoint replacement is implied by local archive creation.
pub fn control(
    credential_file: &Path,
    journal_path: &Path,
    command: ExportCommand,
    budget: Budget,
    out: &mut impl Write,
) -> Res<()> {
    let journal = Journal::open(journal_path)?;
    let request: ExportRequest = journal
        .read("export.json", budget.metadata_bytes)?
        .ok_or_else(|| refused("export request is missing from the journal"))?;
    let connection = connection(credential_file, budget)?;
    if request.version != 1 || request.binding != connection.binding {
        return Err(refused("credential does not match the export journal"));
    }
    let result = connection
        .client
        .portable_export(
            &request.key,
            command,
            &connection.public_key,
            connection.package,
        )
        .map_err(|error| {
            if matches!(command, ExportCommand::Query) {
                Box::new(error) as Box<dyn std::error::Error>
            } else {
                super::uncertain(error, &request.key, journal_path)
            }
        })?;
    if let ExportState::Available { envelope, .. } = &result
        && let Some(original) = journal.read::<Envelope>("envelope.json", budget.metadata_bytes)?
        && original.encode(connection.package)? != envelope.encode(connection.package)?
    {
        return Err(refused("source export identity changed"));
    }
    serde_json::to_writer(&mut *out, &result)?;
    out.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_tests!("portable-export", 0xc10b; ExportRequest, ArchiveFact);
}
