use alt_remote_protocol::{ActorKind, Attribution};
use alt_server::operator::{self, Config, Error, Result};
use clap::{Parser, Subcommand, ValueEnum};
use rand_core::{OsRng, RngCore};
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
    /// Open an existing instance without migration/bootstrap fallback
    Serve,
    /// Verify current schema, persisted mode, receipt key and volume identity
    Health,
    BuildInfo,
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
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn build_info() -> Value {
    json!({"product":"alt-server","version":env!("CARGO_PKG_VERSION"),"build_id":option_env!("ALT_BUILD_ID").unwrap_or("development"),"protocol":1,"schema":3})
}
fn execute(args: &Args) -> Result<Value> {
    if let Action::BuildInfo = &args.command {
        return Ok(build_info());
    }
    if let Action::Secret { kind, output } = &args.command {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
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
                alt_server::serve(listener,factory,options,async move {tokio::select! {_=tokio::signal::ctrl_c()=>{},_=termination.recv()=>{}}}).await.map_err(|_|Error::Unknown)
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
    let token = operator::read_token(args.credential.as_deref().ok_or(Error::Config(
        "--credential is required for this operator action",
    ))?)?;
    let admin = store.authenticate(&token)?;
    let result = match &args.command {
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
    match execute(&args) {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let writing = !matches!(
                args.command,
                Action::Health | Action::BuildInfo | Action::Serve
            );
            let unknown = matches!(error, Error::Unknown)
                || (writing && matches!(error, Error::Store | Error::Database));
            eprintln!(
                "{}",
                json!({"error":{"message":error.to_string(),"outcome":if unknown {"unknown"} else {"not_applied"}}})
            );
            ExitCode::from(if unknown { 75 } else { 1 })
        }
    }
}
