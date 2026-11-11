//! Standalone migration journals bind authenticated server identity, immutable
//! snapshot/target parameters and durable results before reporting completion.
mod export;
mod import;
mod local;
use alt_portable::PackageLimits;
use alt_remote_protocol::PrincipalInfo;
use alt_wire_http::native::{Options, Transport};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};

type Res<T> = Result<T, Box<dyn std::error::Error>>;
fn refused(message: &str) -> Box<dyn std::error::Error> {
    crate::machine::Failure::refusal("portable_refused", message).into()
}
#[derive(Clone, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct Binding {
    origin: String,
    installation_id: String,
    receipt_public_key: String,
    actor: PrincipalInfo,
}
impl PartialEq for Binding {
    fn eq(&self, other: &Self) -> bool {
        self.origin == other.origin
            && self.installation_id == other.installation_id
            && self.receipt_public_key == other.receipt_public_key
            && self.actor.actor_id == other.actor.actor_id
            && self.actor.attribution == other.actor.attribution
    }
}
impl Eq for Binding {}
#[derive(Clone, Copy)]
struct Budget {
    metadata_bytes: usize,
    entries: usize,
    publications: usize,
    blob_bytes: u64,
    total_bytes: u64,
    timeout: Duration,
}
struct Connection {
    client: Transport,
    binding: Binding,
    public_key: [u8; 32],
    package: PackageLimits,
}
fn connection(path: &Path, budget: Budget) -> Res<Connection> {
    if budget.metadata_bytes == 0
        || budget.entries == 0
        || budget.publications == 0
        || budget.blob_bytes == 0
        || budget.total_bytes < budget.blob_bytes
        || budget.timeout.is_zero()
    {
        return Err(refused("invalid local migration budget"));
    }
    let credential = crate::auth::read_credential(path)?;
    let response_bytes = budget
        .metadata_bytes
        .checked_mul(2)
        .and_then(|v| v.checked_add(1024))
        .ok_or_else(|| refused("migration response budget overflow"))?;
    let client = Transport::new(
        &credential.origin,
        credential.access_token,
        Options {
            timeout: budget.timeout,
            response_bytes,
            chunk_bytes: budget.blob_bytes,
            content_bytes: budget.blob_bytes,
            allow_loopback_http: credential.allow_loopback_http,
        },
    )?;
    let native = client.capabilities()?;
    let portable = client.portable_capabilities()?;
    if native.capabilities.installation_id != credential.installation_id
        || portable.installation_id != credential.installation_id
        || native.receipt_public_key != credential.receipt_public_key
        || portable.receipt_public_key != credential.receipt_public_key
    {
        return Err(refused(
            "migration server identity differs from the saved credential",
        ));
    }
    let actor = client.principal()?;
    if actor.actor_id != credential.principal.actor_id
        || actor.attribution != credential.principal.attribution
        || actor.attribution.kind != alt_remote_protocol::ActorKind::Human
    {
        return Err(refused(
            "migration requires the currently authenticated human owner",
        ));
    }
    let public_key: [u8; 32] = alt_input::unhex(&credential.receipt_public_key)?
        .try_into()
        .map_err(|_| refused("invalid migration signing key"))?;
    let package = PackageLimits {
        metadata_bytes: budget.metadata_bytes.min(portable.bounds.metadata_bytes),
        entries: budget.entries.min(portable.bounds.entries),
        publications: budget.publications.min(portable.bounds.publications),
        blob_bytes: budget.blob_bytes.min(portable.bounds.blob_bytes),
        total_bytes: budget.total_bytes.min(portable.bounds.total_bytes),
        protocol: native.capabilities.limits,
    };
    package.validate()?;
    Ok(Connection {
        client,
        binding: Binding {
            origin: credential.origin,
            installation_id: credential.installation_id,
            receipt_public_key: credential.receipt_public_key,
            actor,
        },
        public_key,
        package,
    })
}

fn uncertain(
    error: impl std::fmt::Display,
    key: &alt_remote_protocol::RequestKey,
    journal: &Path,
) -> Box<dyn std::error::Error> {
    let mut failure = crate::machine::Failure::unknown(error.to_string());
    failure.details = Box::new(serde_json::json!({"portable_request":key,"journal":journal}));
    failure.next_action = "Query the original portable export-status or import-status with the same journal before resuming";
    failure.into()
}

pub fn run(op: &crate::cli::PortableOp, out: &mut impl std::io::Write) -> Res<()> {
    use crate::cli::PortableOp;
    use alt_wire_http::native::portable::ExportCommand;
    let options = match op {
        PortableOp::Export { options, .. }
        | PortableOp::ExportStatus { options }
        | PortableOp::FreezeSource { options }
        | PortableOp::ReopenSource { options }
        | PortableOp::RetireExport { options }
        | PortableOp::Import { options, .. }
        | PortableOp::ImportStatus { options }
        | PortableOp::ActivateImport { options }
        | PortableOp::AbandonImport { options } => options,
    };
    let budget = Budget {
        metadata_bytes: options.max_metadata_bytes,
        entries: options.max_entries,
        publications: options.max_publications,
        blob_bytes: options.max_blob_bytes,
        total_bytes: options.max_total_bytes,
        timeout: Duration::from_secs(options.timeout_seconds),
    };
    match op {
        PortableOp::Export {
            repository, output, ..
        } => export::export(
            &options.credential_file,
            repository,
            output,
            &options.journal,
            budget,
            out,
        ),
        PortableOp::Import {
            repository,
            archive,
            mappings,
            source_installation,
            source_repository,
            source_epoch,
            source_key,
            ..
        } => {
            let scope = alt_remote_protocol::Scope {
                installation_id: source_installation.clone(),
                repository_instance_id: source_repository.clone(),
                recovery_epoch: *source_epoch,
            };
            scope.validate()?;
            let key: [u8; 32] = alt_input::unhex(source_key)?
                .try_into()
                .map_err(|_| refused("source signing key must contain exactly 32 bytes"))?;
            import::stage(
                &options.credential_file,
                import::Stage {
                    repository,
                    archive,
                    source: &import::Source {
                        scope,
                        public_key: key,
                    },
                    mappings,
                },
                &options.journal,
                budget,
                out,
            )
        }
        PortableOp::ExportStatus { .. } => export::control(
            &options.credential_file,
            &options.journal,
            ExportCommand::Query,
            budget,
            out,
        ),
        PortableOp::FreezeSource { .. } => export::control(
            &options.credential_file,
            &options.journal,
            ExportCommand::Freeze,
            budget,
            out,
        ),
        PortableOp::ReopenSource { .. } => export::control(
            &options.credential_file,
            &options.journal,
            ExportCommand::Reopen,
            budget,
            out,
        ),
        PortableOp::RetireExport { .. } => export::control(
            &options.credential_file,
            &options.journal,
            ExportCommand::Retire,
            budget,
            out,
        ),
        PortableOp::ImportStatus { .. } => import::control(
            &options.credential_file,
            &options.journal,
            import::Command::Query,
            budget,
            out,
        ),
        PortableOp::ActivateImport { .. } => import::control(
            &options.credential_file,
            &options.journal,
            import::Command::Activate,
            budget,
            out,
        ),
        PortableOp::AbandonImport { .. } => import::control(
            &options.credential_file,
            &options.journal,
            import::Command::Abandon,
            budget,
            out,
        ),
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_tests!("portable-mod", 0xc10d; Binding);
}
