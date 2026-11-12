//! A copy of part of a real operator command line, so that the outputs
//! recorded in `fixtures/server.txt` apply to it word for word.
#![allow(dead_code)]

use std::path::PathBuf;

use alt_args::{Parser, Subcommand, ValueEnum};

pub const SCHEMA_VERSION: u32 = 20;

#[derive(Parser)]
#[command(
    name = "alt-server",
    version,
    about = "alt native and Git data plane and explicit instance operator"
)]
pub struct Args {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// Private file containing an already issued administrator token
    #[arg(long, global = true)]
    pub credential: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Action,
}

#[derive(Subcommand)]
pub enum Action {
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
        kind: SecretKind,
        #[arg(long)]
        output: PathBuf,
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
    /// Open an existing instance without migration/bootstrap fallback
    Serve,
    /// Verify current schema, persisted mode, receipt key and volume identity
    Health,
    /// Read-only administrator counters; does not perform a storage scrub
    Status,
    /// Read administrator operation history without secrets or action details
    Audit {
        #[arg(long)]
        before: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Encrypted physical backup, including credentials and the receipt key
    Backup {
        #[arg(long, default_value_t = SCHEMA_VERSION)]
        schema: u32,
        #[arg(long)]
        recipient: String,
        #[arg(long)]
        output: PathBuf,
    },
    ReviewGrant {
        repository: String,
        actor: String,
        #[arg(long, action = set)]
        allow: bool,
    },
    Collect {
        repository: String,
        #[arg(long, default_value = "100")]
        limit: u32,
    },
}

#[derive(Clone, Debug, PartialEq, ValueEnum)]
pub enum SecretKind {
    ReceiptKey,
    Token,
}
