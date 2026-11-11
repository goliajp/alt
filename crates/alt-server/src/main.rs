use alt_remote_protocol::{ActorKind, Attribution};
use alt_server::operator::{self, Config, Error, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Parser)]
#[command(
    name = "alt-server",
    version,
    about = "alt native and Git data plane and explicit instance operator"
)]
struct Args {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Private file containing an already issued administrator token
    #[arg(long, global = true)]
    credential: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Restore or reconcile one frozen Cloud recovery into a new incarnation
    CloudRestore {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        assets: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8081")]
        listen: std::net::SocketAddr,
        #[arg(long, default_value = "ALT_SERVER_DATABASE")]
        database_environment: String,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        source_fenced: bool,
    },
    /// Initialize or resume exactly one frozen Cloud provision specification
    CloudInitialize {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        assets: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8081")]
        listen: std::net::SocketAddr,
        #[arg(long, default_value = "ALT_SERVER_DATABASE")]
        database_environment: String,
    },
    /// Generate and fsync a secret before registering it with the database
    Secret {
        #[arg(value_enum)]
        kind: SecretKind,
        #[arg(long)]
        output: PathBuf,
    },
    /// Initialize a dedicated database and empty byte volume explicitly
    Initialize {
        #[arg(long)]
        resume: bool,
    },
    /// Install the first self-hosted administrator using a saved secret
    Bootstrap {
        actor: String,
        #[arg(long)]
        secret: PathBuf,
        #[arg(long)]
        expires_unix_ms: u64,
    },
    /// Explicitly install one operator-configured cross-product pairing
    LinkInstall {
        repository: String,
        epoch: u64,
        channel: String,
    },
    /// Disable a pairing without removing its historical signed facts
    LinkDisable {
        repository: String,
        epoch: u64,
        channel: String,
    },
    /// Open an existing instance without migration/bootstrap fallback
    Serve,
    /// Verify current schema, persisted mode, receipt key and volume identity
    Health,
    BuildInfo,
    /// Read-only administrator counters; does not perform a storage scrub
    Status,
    /// Read administrator operation history without secrets or action details
    Audit {
        #[arg(long)]
        before: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Explicit checksummed schema upgrade; save the next config before commit
    Migrate {
        #[arg(long)]
        from_schema: u32,
        #[arg(long)]
        output_config: PathBuf,
    },
    /// Encrypted physical backup, including credentials and the receipt key
    Backup {
        #[arg(long, default_value_t = alt_server_store::SCHEMA_VERSION)]
        schema: u32,
        #[arg(long)]
        recipient: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Authenticate and decrypt every backup entry without restoring a database
    BackupVerify {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        trusted_receipt_key: String,
        #[arg(long)]
        maximum_bytes: u64,
    },
    /// Restore to a dedicated empty database and absent volume/key paths
    Restore {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        trusted_receipt_key: String,
        #[arg(long)]
        maximum_bytes: u64,
        /// Attest that the old instance is externally stopped and fenced
        #[arg(long)]
        source_fenced: bool,
    },
    RepositoryCreate {
        id: String,
        #[arg(long)]
        quota_bytes: u64,
    },
    /// Replace the named actor's explicit repository grants from a JSON file
    Grants {
        repository: String,
        actor: String,
        #[arg(long)]
        file: PathBuf,
    },
    ReviewGrant {
        repository: String,
        actor: String,
        #[arg(long,action=clap::ArgAction::Set)]
        allow: bool,
    },
    CollectorPolicy {
        repository: String,
        #[arg(long)]
        file: PathBuf,
    },
    BranchPolicy {
        repository: String,
        #[arg(long)]
        file: PathBuf,
    },
    DefaultBranch {
        repository: String,
        branch: String,
    },
    TokenIssue {
        actor: String,
        #[arg(long)]
        secret: PathBuf,
        #[arg(long)]
        expires_unix_ms: u64,
        #[arg(long)]
        administrator: bool,
        #[arg(long)]
        attribution: Option<PathBuf>,
    },
    /// Persist the configured origin before enabling browser traffic
    BrowserEnable,
    /// Create a local browser user from a private password file
    UserCreate {
        actor: String,
        #[arg(long)]
        login: String,
        #[arg(long)]
        display_name: String,
        #[arg(long)]
        password_file: PathBuf,
        #[arg(long)]
        administrator: bool,
    },
    UserResetPassword {
        actor: String,
        #[arg(long)]
        password_file: PathBuf,
    },
    /// Recover an existing local administrator with physical instance access
    RecoverLocalAdministrator {
        actor: String,
        #[arg(long)]
        password_file: PathBuf,
        #[arg(long)]
        reason: String,
    },
    /// Recover an existing local human administrator with a short-lived token
    RecoverLocalToken {
        actor: String,
        #[arg(long)]
        secret: PathBuf,
        #[arg(long)]
        expires_unix_ms: u64,
        #[arg(long)]
        reason: String,
    },
    TokenRevoke {
        #[arg(long)]
        secret: PathBuf,
    },
    Collect {
        repository: String,
        #[arg(long, default_value = "100")]
        limit: u32,
    },
}
#[derive(Clone, ValueEnum)]
enum SecretKind {
    ReceiptKey,
    Token,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Grants {
    read: bool,
    publish_records: bool,
    grants: Vec<alt_repository_service::Grant>,
}
fn json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&operator::read_regular(path, 4 * 1024 * 1024, false)?)
        .map_err(|_| Error::Config("operator input JSON"))
}
fn password_file(path: &Path) -> Result<String> {
    let raw = operator::read_regular(path, 1025, true)?;
    let value = std::str::from_utf8(&raw).map_err(|_| Error::Config("password encoding"))?;
    Ok(value.strip_suffix('\n').unwrap_or(value).to_owned())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn public_key(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Config(
            "trusted receipt key must be 64 lowercase hex digits",
        ));
    }
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| Error::Config("trusted receipt key"))?;
    }
    Ok(key)
}
fn build_info() -> Value {
    json!({"product":"alt-server","version":env!("CARGO_PKG_VERSION"),"build_id":option_env!("ALT_BUILD_ID").unwrap_or("development"),"protocol":1,"schema":alt_server_store::SCHEMA_VERSION})
}
fn execute(args: &Args) -> Result<Value> {
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
        let spec: golia_platform_contract::lifecycle::RecoverySpec =
            serde_json::from_slice(&operator::read_regular(spec, 65536, true)?)
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
            operator::backup::restore(&config, verified, *source_fenced)?;
        }
        return Ok(
            json!({"restored":true,"instance":config.instance,"manifest_digest":spec.backup.manifest_digest,"old_credentials_revoked":true}),
        );
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
        return Ok(json!({"initialized":true,"instance":config.instance,
            "configuration":directory.join("config.json"),
            "spec_digest":config.cloud.as_ref().and_then(|c| c.provision_digest.as_ref())}));
    }
    if let Action::BuildInfo = &args.command {
        return Ok(build_info());
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
        return Ok(
            json!({"verified":true,"restored":false,"manifest_digest":verified.manifest_digest,"catalog":verified.manifest.catalog}),
        );
    }
    if let Action::Secret { kind, output } = &args.command {
        let mut seed = [0u8; 32];
        alt_os::random::fill(&mut seed);
        let bytes = match kind {
            SecretKind::ReceiptKey => seed.to_vec(),
            SecretKind::Token => format!("alt1_{}\n", hex(&seed)).into_bytes(),
        };
        operator::create_private(output, &bytes)?;
        return Ok(
            json!({"created":output,"kind":match kind {SecretKind::ReceiptKey=>"receipt_key",SecretKind::Token=>"token"}}),
        );
    }
    let config = Config::read(
        args.config
            .as_deref()
            .ok_or(Error::Config("--config is required"))?,
    )?;
    match &args.command {
        Action::Migrate {
            from_schema,
            output_config,
        } => {
            let next = config.migrate(*from_schema, output_config)?;
            return Ok(
                json!({"migrated":true,"instance":next.instance,"configuration":output_config,"schema":alt_server_store::SCHEMA_VERSION}),
            );
        }
        Action::Backup {
            schema,
            recipient,
            output,
        } => {
            let manifest = operator::backup::create(&config, *schema, recipient, output)?;
            return Ok(
                json!({"backup_created":true,"restored":false,"operation_id":manifest.operation_id,"output":output,"catalog":manifest.catalog,"payload":manifest.payload}),
            );
        }
        Action::Restore {
            input,
            identity,
            trusted_receipt_key,
            maximum_bytes,
            source_fenced,
        } => {
            if !source_fenced {
                return Err(Error::Config(
                    "--source-fenced is required for disaster recovery",
                ));
            }
            let verified = operator::backup::verify(
                input,
                identity,
                public_key(trusted_receipt_key)?,
                *maximum_bytes,
            )?;
            let digest = verified.manifest_digest.clone();
            let catalog = operator::backup::restore(&config, verified, *source_fenced)?;
            return Ok(
                json!({"restored":true,"manifest_digest":digest,"catalog":catalog,"old_credentials_revoked":true}),
            );
        }
        _ => (),
    }
    if let Action::Initialize { resume } = &args.command {
        let store = config.initialize(*resume)?;
        return Ok(
            json!({"initialized":true,"instance":store.spec(),"receipt_public_key":hex(config.signing_key()?.verifying_key().as_bytes()),"build":build_info()}),
        );
    }
    // Check before starting an async runtime: PostgreSQL's synchronous client
    // must also be dropped outside the runtime.
    let mut store = config.open()?;
    match &args.command {
        Action::Serve => {
            drop(store);
            let factory = config.factory()?;
            let options = config.http_options();
            let browser = config.browser_options()?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(config.http.workers.min(16))
                .enable_all()
                .build()
                .map_err(|_| Error::Config("async runtime"))?;
            runtime.block_on(async {
                let listener=tokio::net::TcpListener::bind(config.listen).await.map_err(|_|Error::Config("listener unavailable"))?;
                let address=listener.local_addr().map_err(|_|Error::Config("listener identity"))?;
                let line=json!({"event":"listening","address":address,"instance":config.instance,"build":build_info()});
                println!("{line}");std::io::stdout().flush().map_err(|_|Error::File)?;
                let mut termination=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|_|Error::Config("termination signal"))?;
                alt_server::serve_with_browser(listener,factory,options,browser,async move {tokio::select! {_=tokio::signal::ctrl_c()=>{},_=termination.recv()=>{}}}).await.map_err(|_|Error::Unknown)
            })?;
            return Ok(json!({"event":"stopped"}));
        }
        Action::Health => {
            return Ok(
                json!({"ready":true,"instance":store.spec(),"build":build_info(),"receipt_public_key":hex(config.signing_key()?.verifying_key().as_bytes())}),
            );
        }
        Action::Bootstrap {
            actor,
            secret,
            expires_unix_ms,
        } => {
            store.bootstrap_admin_with_token(
                actor,
                *expires_unix_ms,
                &operator::read_token(secret)?,
            )?;
            return Ok(json!({"actor":actor,"bootstrapped":true}));
        }
        _ => (),
    }
    if let Action::RecoverLocalAdministrator {
        actor,
        password_file: path,
        reason,
    } = &args.command
    {
        store.recover_local_administrator(actor, &password_file(path)?, reason)?;
        return Ok(json!({"actor":actor,"recovered":true,"old_credentials_revoked":true}));
    }
    if let Action::RecoverLocalToken {
        actor,
        secret,
        expires_unix_ms,
        reason,
    } = &args.command
    {
        store.recover_local_token(
            actor,
            &operator::read_token(secret)?,
            *expires_unix_ms,
            reason,
        )?;
        return Ok(
            json!({"actor":actor,"recovered":true,"old_credentials_revoked":true,"expires_unix_ms":expires_unix_ms.to_string()}),
        );
    }
    let token = config.read_credential(args.credential.as_deref().ok_or(Error::Config(
        "--credential is required for this operator action",
    ))?)?;
    let admin = config.factory()?.authenticate(&mut store, &token)?;
    let result = match &args.command {
        Action::LinkInstall {
            repository,
            epoch,
            channel,
        } => {
            let scope = alt_remote_protocol::Scope {
                installation_id: config.instance.installation_id.clone(),
                repository_instance_id: repository.clone(),
                recovery_epoch: *epoch,
            };
            let factory = config.factory()?;
            let runtime = factory
                .change_links
                .iter()
                .find(|c| c.matches(&scope, channel))
                .ok_or(Error::Config("change-link is not configured"))?;
            runtime.install(&factory, &token)?;
            json!({"repository":repository,"channel":channel,"installed":true})
        }
        Action::LinkDisable {
            repository,
            epoch,
            channel,
        } => {
            let scope = alt_remote_protocol::Scope {
                installation_id: config.instance.installation_id.clone(),
                repository_instance_id: repository.clone(),
                recovery_epoch: *epoch,
            };
            store.disable_change_channel(&admin, &scope, channel)?;
            json!({"repository":repository,"channel":channel,"disabled":true})
        }
        Action::Status => {
            serde_json::to_value(store.instance_status(&admin)?).map_err(|_| Error::Store)?
        }
        Action::Audit { before, limit } => {
            serde_json::to_value(store.instance_audit(&admin, *before, *limit)?)
                .map_err(|_| Error::Store)?
        }
        Action::RepositoryCreate { id, quota_bytes } => {
            json!({"repository":store.create_repository(&admin,id,*quota_bytes)?})
        }
        Action::Grants {
            repository,
            actor,
            file,
        } => {
            let grant: Grants = json_file(file)?;
            store.set_grants(
                &admin,
                repository,
                actor,
                grant.read,
                grant.publish_records,
                &grant.grants,
            )?;
            json!({"repository":repository,"actor":actor,"grants_updated":true})
        }
        Action::ReviewGrant {
            repository,
            actor,
            allow,
        } => {
            store.set_review_grant(&admin, repository, actor, *allow)?;
            json!({"repository":repository,"actor":actor,"can_review":allow})
        }
        Action::CollectorPolicy { repository, file } => {
            store.set_collector_policy(&admin, repository, &json_file(file)?)?;
            json!({"repository":repository,"collector_policy_updated":true})
        }
        Action::BranchPolicy { repository, file } => {
            store.set_policies(
                &admin,
                repository,
                &json_file::<Vec<alt_repository_service::RefPolicy>>(file)?,
            )?;
            json!({"repository":repository,"branch_policy_updated":true})
        }
        Action::DefaultBranch { repository, branch } => {
            store.set_default_branch(&admin, repository, branch)?;
            json!({"repository":repository,"default_branch":branch})
        }
        Action::TokenIssue {
            actor,
            secret,
            expires_unix_ms,
            administrator,
            attribution,
        } => {
            let attribution = match attribution {
                Some(path) => json_file(path)?,
                None => Attribution {
                    kind: ActorKind::Human,
                    on_behalf_of: None,
                    delegation_id: None,
                    agent_session: None,
                },
            };
            store.issue_local_token_with_secret(
                &admin,
                &operator::read_token(secret)?,
                actor,
                &attribution,
                *expires_unix_ms,
                *administrator,
            )?;
            json!({"actor":actor,"credential_registered":true,"expires_unix_ms":expires_unix_ms.to_string()})
        }
        Action::BrowserEnable => {
            let browser = config
                .browser
                .as_ref()
                .ok_or(Error::Config("browser configuration required"))?;
            let origin = alt_browser_auth::PublicOrigin::parse(&browser.public_origin)
                .map_err(|_| Error::Config("browser origin"))?;
            store.enable_browser(&admin, &origin)?;
            json!({"browser_enabled":true,"public_origin":origin.as_str()})
        }
        Action::UserCreate {
            actor,
            login,
            display_name,
            password_file,
            administrator,
        } => {
            let raw = operator::read_regular(password_file, 1025, true)?;
            let password = std::str::from_utf8(&raw)
                .map_err(|_| Error::Config("password encoding"))?
                .strip_suffix('\n')
                .unwrap_or(
                    std::str::from_utf8(&raw).map_err(|_| Error::Config("password encoding"))?,
                );
            json!({"user":store.create_local_user(&admin, actor, login, display_name, password, *administrator)?})
        }
        Action::UserResetPassword {
            actor,
            password_file: path,
        } => {
            store.reset_local_password(&admin, actor, &password_file(path)?)?;
            json!({"actor":actor,"password_reset":true,"old_credentials_revoked":true})
        }
        Action::TokenRevoke { secret } => {
            store.revoke(&admin, &operator::read_token(secret)?)?;
            json!({"revoked":true})
        }
        Action::Collect { repository, limit } => {
            json!({"repository":repository,"removed":store.collect_orphans(&admin,repository,*limit)?})
        }
        _ => return Err(Error::Config("unsupported operator action")),
    };
    Ok(result)
}
fn main() -> ExitCode {
    let args = Args::parse();
    let _logging = if matches!(args.command, Action::Serve) {
        match alt_server_telemetry::install_logging(std::io::stderr(), 4096) {
            Ok(logging) => Some(logging),
            Err(_) => {
                eprintln!(
                    "{}",
                    json!({"error":{"message":"Operational logging could not be initialized","outcome":"not_applied"}})
                );
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    match execute(&args) {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let writing = !matches!(
                args.command,
                Action::Health
                    | Action::BuildInfo
                    | Action::Serve
                    | Action::Status
                    | Action::Audit { .. }
                    | Action::BackupVerify { .. }
            );
            let unknown = matches!(error, Error::Unknown | Error::RestoreStage(_))
                || (writing && matches!(error, Error::Store | Error::Database));
            eprintln!(
                "{}",
                json!({"error":{"message":error.to_string(),"outcome":if unknown {"unknown"} else {"not_applied"}}})
            );
            ExitCode::from(if unknown { 75 } else { 1 })
        }
    }
}
