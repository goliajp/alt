use alt_args::Parser;
use alt_json::{Value, json};
use alt_remote_protocol::{ActorKind, Attribution};
use alt_server::operator::{self, Config, Error, Result};
mod action;
mod administered;
mod grants;
mod unconfigured;
use action::{Action, SecretKind};
use grants::Grants;
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
fn json_file<T: alt_json::FromJson>(path: &Path) -> Result<T> {
    alt_json::from_slice(&operator::read_regular(path, 4 * 1024 * 1024, false)?)
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
    if let Some(done) = unconfigured::execute(args)? {
        return Ok(done);
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
            with_unread,
        } => {
            let manifest =
                operator::backup::create(&config, *schema, recipient, output, *with_unread)?;
            return Ok(
                json!({"backup_created":true,"restored":false,"operation_id":manifest.operation_id,"output":output,"catalog":manifest.catalog,"payload":manifest.payload,"unread":manifest.unread}),
            );
        }
        Action::Restore {
            input,
            identity,
            trusted_receipt_key,
            maximum_bytes,
            source_fenced,
            with_unread,
        } => {
            if !source_fenced {
                return Err(Error::Config(
                    "--source-fenced is required for disaster recovery",
                ));
            }
            // read out beside the bytes it is restored to
            let work = operator::backup::beside(&config.bytes_directory)?;
            let verified = operator::backup::verify(
                (input, work),
                identity,
                public_key(trusted_receipt_key)?,
                *maximum_bytes,
            )?;
            let digest = verified.manifest_digest.clone();
            let (catalog, unread) =
                operator::backup::restore(&config, verified, *source_fenced, *with_unread)?;
            return Ok(
                json!({"restored":true,"manifest_digest":digest,"catalog":catalog,"old_credentials_revoked":true,"unread":unread}),
            );
        }
        // read alone, with no credential: it is run with this version against
        // an instance of an older schema, before that is upgraded
        Action::RefusedTrees { repository } => {
            let mut store = operator::scrub::opened_as_it_is(&config)?;
            return operator::scrub::refused_trees(&mut store, repository.as_deref());
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
            return serve(&config);
        }
        Action::Health => {
            return Ok(
                json!({"ready":true,"instance":store.spec(),"build":build_info(),"receipt_public_key":hex(config.signing_key()?.verifying_key().as_bytes()),"requests_awaiting_settled_at":store.requests_without_moment()?}),
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
    administered::execute(args, &config, &mut store, (&admin, &token))
}
/// Serves the instance of `config` until it is told to stop.
fn serve(config: &Config) -> Result<Value> {
    let factory = config.factory()?;
    let options = config.serving_options()?;
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
        alt_server::serve_with_browser(listener,factory,options,browser,async move {tokio::select! {_=tokio::signal::ctrl_c()=>{},_=termination.recv()=>{}}}).await.map_err(|error|Error::Serve(error.to_string()))
    })?;
    Ok(json!({"event":"stopped"}))
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
        Err(Error::Partly(told, unknown)) => {
            println!("{told}");
            ExitCode::from(if unknown { 75 } else { 1 })
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
                    | Action::RefusedTrees { .. }
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
