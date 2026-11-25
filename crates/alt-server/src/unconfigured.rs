//! The operator actions that read no configuration of an instance: those that
//! make one, and those that are about a file or about this program.

use super::*;

/// What the action of `args` gives, where it is one of these.
pub(super) fn execute(args: &Args) -> Result<Option<Value>> {
    if let Action::CloudRestore {
        spec,
        directory,
        assets,
        listen,
        database_environment,
        input,
        identity,
        source_fenced,
    } = &args.command
    {
        if !source_fenced {
            return Err(Error::Config(
                "--source-fenced is required for disaster recovery",
            ));
        }
        let alt_json_serde::Serde::<golia_platform_contract::lifecycle::RecoverySpec>(spec) =
            alt_json::from_slice(&operator::read_regular(spec, 65536, true)?)
                .map_err(|_| Error::Config("recovery specification"))?;
        spec.validate()
            .map_err(|_| Error::Config("recovery specification"))?;
        let verified = operator::backup::verify(
            input,
            identity,
            public_key(&spec.backup.receipt_public_key)?,
            spec.backup.maximum_bytes.0,
        )?;
        let instance = &verified.manifest.catalog.instance;
        if verified.manifest_digest != spec.backup.manifest_digest
            || instance.installation_id != spec.target.audience.installation_id.as_str()
            || instance.workspace_id != spec.target.audience.workspace_id.as_str()
            || instance.runtime_generation != spec.backup.generation.0
            || instance.deployment_mode != alt_remote_protocol::DeploymentMode::Cloud
        {
            return Err(Error::Config("recovery backup differs from frozen intent"));
        }
        let config = operator::provision::prepare_recovery(
            &spec,
            directory,
            assets,
            *listen,
            database_environment,
        )?;
        if config
            .receipt_key_file
            .try_exists()
            .map_err(|_| Error::File)?
        {
            let mut store = config
                .open()
                .map_err(|_| Error::RestoreStage("existing recovery verification"))?;
            store
                .check_restored_backup(&spec.backup.manifest_digest)
                .map_err(|_| Error::RestoreStage("existing recovery receipt"))?;
        } else {
            operator::backup::restore(&config, verified, *source_fenced, false)?;
        }
        return Ok(Some(
            json!({"restored":true,"instance":config.instance,"manifest_digest":spec.backup.manifest_digest,"old_credentials_revoked":true}),
        ));
    }
    if let Action::CloudInitialize {
        spec,
        directory,
        assets,
        listen,
        database_environment,
    } = &args.command
    {
        let config = operator::provision::initialize(
            spec,
            directory,
            assets,
            *listen,
            database_environment,
        )?;
        return Ok(Some(json!({"initialized":true,"instance":config.instance,
            "configuration":directory.join("config.json"),
            "spec_digest":config.cloud.as_ref().and_then(|c| c.provision_digest.as_ref())})));
    }
    if let Action::BuildInfo = &args.command {
        return Ok(Some(build_info()));
    }
    if let Action::BackupVerify {
        input,
        identity,
        trusted_receipt_key,
        maximum_bytes,
    } = &args.command
    {
        let verified = operator::backup::verify(
            input,
            identity,
            public_key(trusted_receipt_key)?,
            *maximum_bytes,
        )?;
        return Ok(Some(
            json!({"verified":true,"restored":false,"manifest_digest":verified.manifest_digest,"catalog":verified.manifest.catalog}),
        ));
    }
    if let Action::Secret { kind, output } = &args.command {
        let mut seed = [0u8; 32];
        alt_os::random::fill(&mut seed);
        let bytes = match kind {
            SecretKind::ReceiptKey => seed.to_vec(),
            SecretKind::Token => format!("alt1_{}\n", hex(&seed)).into_bytes(),
        };
        operator::create_private(output, &bytes)?;
        return Ok(Some(
            json!({"created":output,"kind":match kind {SecretKind::ReceiptKey=>"receipt_key",SecretKind::Token=>"token"}}),
        ));
    }
    Ok(None)
}
