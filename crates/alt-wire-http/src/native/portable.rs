//! Portable HTTP shares authenticated origin handling and bounded networking
//! with the native transport. Callers persist the exact intent before writes.
use super::{Error, Result, Transport, bounded, digest, id, scope_path};
use alt_portable::{
    Blob, Envelope, ImportIntent, PackageLimits,
    http::{BeginImport, Capabilities, ExportState, ImportState},
};
use alt_remote_protocol::{RequestKey, Scope};
use std::io::{Read, Write};

#[derive(Clone, Copy)]
pub enum ExportCommand {
    Prepare,
    Query,
    Freeze,
    Reopen,
    Retire,
}
impl ExportCommand {
    fn suffix(self) -> &'static str {
        match self {
            Self::Prepare | Self::Query => "",
            Self::Freeze => "/freeze",
            Self::Reopen => "/reopen",
            Self::Retire => "/retire",
        }
    }
}
fn export_path(scope: &Scope, export: &str) -> Result<String> {
    id(export)?;
    Ok(format!("{}/portable/exports/{export}", scope_path(scope)?))
}
fn import_path(key: &RequestKey) -> Result<String> {
    key.validate()?;
    Ok(format!(
        "{}/portable/imports/{}/{}",
        scope_path(&key.scope)?,
        key.actor_id,
        key.request_id
    ))
}
fn key_bytes(value: &str) -> Result<[u8; 32]> {
    digest(value)?;
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| Error::Invalid("portable receipt key"))?;
    }
    Ok(bytes)
}
fn state(value: &ImportState, intent: &ImportIntent, limits: PackageLimits) -> Result<()> {
    let expected = intent.digest(limits).map_err(Error::Portable)?;
    match value {
        ImportState::Receiving {
            digest: actual,
            missing,
            ..
        } => {
            if *actual != expected || missing.len() > limits.entries {
                return Err(Error::Invalid("portable import response intent"));
            }
            let mut previous: Option<&str> = None;
            let mut total = 0u64;
            for blob in missing {
                digest(&blob.blake3)?;
                if previous.is_some_and(|p| p >= blob.blake3.as_str())
                    || blob.bytes > limits.blob_bytes
                {
                    return Err(Error::Invalid("portable missing content selection"));
                }
                total = total
                    .checked_add(blob.bytes)
                    .ok_or(Error::Limit("portable missing bytes"))?;
                previous = Some(&blob.blake3);
            }
            if total > limits.total_bytes {
                return Err(Error::Limit("portable missing bytes"));
            }
        }
        ImportState::Committed { receipt } => {
            receipt
                .verify(intent, &key_bytes(&intent.target_public_key)?, limits)
                .map_err(Error::Portable)?;
        }
        ImportState::Abandoned { digest: actual } if *actual == expected => {}
        ImportState::Unknown { key } | ImportState::NotSeen { key } if *key == intent.key => {}
        _ => return Err(Error::Invalid("portable import response identity")),
    }
    Ok(())
}
impl Transport {
    pub fn portable_capabilities(&self) -> Result<Capabilities> {
        let value: Capabilities = self.json("GET", "/v1/portable/capabilities", b"", None)?;
        if value.version != 1 {
            return Err(Error::Invalid("portable protocol version"));
        }
        id(&value.installation_id)?;
        key_bytes(&value.receipt_public_key)?;
        Ok(value)
    }
    pub fn portable_export(
        &self,
        key: &RequestKey,
        command: ExportCommand,
        pinned_key: &[u8; 32],
        limits: PackageLimits,
    ) -> Result<ExportState> {
        key.validate()?;
        let read = matches!(command, ExportCommand::Query);
        let path = format!(
            "{}{}",
            export_path(&key.scope, &key.request_id)?,
            command.suffix()
        );
        let value: ExportState = self.json(
            if read { "GET" } else { "POST" },
            &path,
            b"",
            (!read).then_some(key),
        )?;
        match &value {
            ExportState::Available { envelope, .. } => {
                envelope
                    .verify(&key.scope, pinned_key, limits)
                    .map_err(Error::Portable)?;
                if envelope.catalog.export_id != key.request_id {
                    return Err(Error::Invalid("portable export response identity"));
                }
            }
            ExportState::Preparing {
                scope,
                export_id,
                catalog_digest,
                ..
            }
            | ExportState::Retired {
                scope,
                export_id,
                catalog_digest,
            } => {
                digest(catalog_digest)?;
                if *scope != key.scope || *export_id != key.request_id {
                    return Err(Error::Invalid("portable export response identity"));
                }
            }
            ExportState::Unknown { scope, export_id }
            | ExportState::NotSeen { scope, export_id } => {
                if *scope != key.scope || *export_id != key.request_id {
                    return Err(Error::Invalid("portable export response identity"));
                }
            }
        }
        Ok(value)
    }
    pub fn portable_download(
        &self,
        scope: &Scope,
        export: &str,
        blob: &Blob,
        output: &mut dyn Write,
    ) -> Result<()> {
        digest(&blob.blake3)?;
        self.download_path(
            &format!("{}/content/{}", export_path(scope, export)?, blob.blake3),
            &blob.blake3,
            blob.bytes,
            output,
        )
    }
    pub fn portable_import_begin(
        &self,
        intent: &ImportIntent,
        envelope: &Envelope,
        limits: PackageLimits,
    ) -> Result<ImportState> {
        intent
            .validate_envelope(envelope, limits)
            .map_err(Error::Portable)?;
        let body = self.encode(&BeginImport {
            intent: intent.clone(),
            envelope: envelope.clone(),
        })?;
        let value = self.json("POST", &import_path(&intent.key)?, &body, Some(&intent.key))?;
        state(&value, intent, limits)?;
        Ok(value)
    }
    pub fn portable_import_status(
        &self,
        intent: &ImportIntent,
        limits: PackageLimits,
    ) -> Result<ImportState> {
        let value = self.json("GET", &import_path(&intent.key)?, b"", None)?;
        state(&value, intent, limits)?;
        Ok(value)
    }
    pub fn portable_import_activate(
        &self,
        intent: &ImportIntent,
        limits: PackageLimits,
    ) -> Result<ImportState> {
        let value = self.json(
            "POST",
            &format!("{}/activate", import_path(&intent.key)?),
            &self.encode(intent)?,
            Some(&intent.key),
        )?;
        state(&value, intent, limits)?;
        if !matches!(value, ImportState::Committed { .. }) {
            return Err(Error::Invalid("portable activation response"));
        }
        Ok(value)
    }
    pub fn portable_import_abandon(
        &self,
        intent: &ImportIntent,
        limits: PackageLimits,
    ) -> Result<ImportState> {
        let value = self.json(
            "POST",
            &format!("{}/abandon", import_path(&intent.key)?),
            b"",
            Some(&intent.key),
        )?;
        state(&value, intent, limits)?;
        if !matches!(value, ImportState::Abandoned { .. }) {
            return Err(Error::Invalid("portable abandonment response"));
        }
        Ok(value)
    }
    /// The reader is a preverified immutable local blob, never an arbitrary
    /// URL. Server and client both verify the bytes actually consumed.
    pub fn portable_upload(
        &self,
        key: &RequestKey,
        blob: &Blob,
        input: &mut dyn Read,
    ) -> Result<()> {
        digest(&blob.blake3)?;
        if blob.bytes > self.options.content_bytes {
            return Err(Error::Limit("portable upload bytes"));
        }
        let path = format!("{}/content/{}", import_path(key)?, blob.blake3);
        let request = self
            .agent
            .put(&format!("{}{path}", self.origin))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept-Encoding", "identity")
            .set("Content-Type", "application/octet-stream")
            .set("Content-Length", &blob.bytes.to_string());
        let mut reader = HashReader {
            input,
            hash: alt_hash::blake3::Hasher::new(),
            count: 0,
        };
        let response = match request.send(&mut reader) {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(_) => {
                return Err(Error::OutcomeUnknown {
                    key: Box::new(key.clone()),
                });
            }
        };
        if response.status() != 200 {
            let status = response.status();
            let failure = bounded(response, self.options.response_bytes)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .map(Box::new);
            return Err(Error::Rejected { status, failure });
        }
        if reader.count != blob.bytes
            || reader.hash.finalize().to_hex().as_str() != blob.blake3
            || response.header("Content-Encoding").is_some()
            || response
                .header("Content-Type")
                .and_then(|s| s.split(';').next())
                != Some("application/json")
        {
            return Err(Error::OutcomeUnknown {
                key: Box::new(key.clone()),
            });
        }
        let decode = || -> Result<()> {
            let value: serde_json::Value =
                serde_json::from_slice(&bounded(response, self.options.response_bytes)?)
                    .map_err(|_| Error::Invalid("portable upload response"))?;
            if value["blake3"] != blob.blake3 || value["verified"] != true {
                return Err(Error::Invalid("portable upload response identity"));
            }
            Ok(())
        };
        decode().map_err(|_| Error::OutcomeUnknown {
            key: Box::new(key.clone()),
        })
    }
}
struct HashReader<'a> {
    input: &'a mut dyn Read,
    hash: alt_hash::blake3::Hasher,
    count: u64,
}
impl Read for HashReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let n = self.input.read(output)?;
        self.hash.update(&output[..n]);
        self.count = self
            .count
            .checked_add(n as u64)
            .ok_or_else(|| std::io::Error::other("portable upload length overflow"))?;
        Ok(n)
    }
}
