//! The subcommands of `auth`, `runner`, `request`, `operation` and
//! `portable`.

use alt_args::Subcommand;

#[derive(Subcommand, Debug)]
pub enum AuthOp {
    /// Give an agent a short credential with explicit repository and write bounds
    Delegate {
        #[arg(long)]
        credential_file: std::path::PathBuf,
        #[arg(long)]
        agent_file: std::path::PathBuf,
        /// JSON purpose, agent_session, lifetime_seconds and scope
        #[arg(long, required_unless_present = "resume", conflicts_with = "resume")]
        plan: Option<std::path::PathBuf>,
        /// Retry the same privately saved delegation after interruption
        #[arg(long)]
        resume: bool,
    },
    /// List agent credentials belonging to the current human account
    Delegations {
        #[arg(long)]
        credential_file: std::path::PathBuf,
        #[arg(long)]
        after: Option<String>,
    },
    /// Revoke one agent credential owned by the current human account
    RevokeAgent {
        #[arg(long)]
        credential_file: std::path::PathBuf,
        #[arg(long)]
        credential_id: String,
    },
    /// Save a private, origin-bound credential after explicit browser consent
    Login {
        #[arg(long)]
        server: String,
        #[arg(long)]
        credential_file: std::path::PathBuf,
        /// Continue the same saved request after interruption
        #[arg(long)]
        resume: bool,
        /// Save the request and print its public confirmation URL without polling
        #[arg(long)]
        no_wait: bool,
        #[arg(long)]
        allow_loopback_http: bool,
    },
    /// Check the current principal and installation without printing secrets
    Status {
        #[arg(long)]
        credential_file: std::path::PathBuf,
    },
    /// Revoke the saved credential before removing its private file
    Logout {
        #[arg(long)]
        credential_file: std::path::PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub enum RunnerOp {
    Probe {
        #[arg(long)]
        bindings: String,
        #[arg(long)]
        environment: String,
    },
    Execute {
        spec: std::path::PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub enum RequestOp {
    /// List requests without a durable completion
    Pending,
    /// Inspect request scope, completion, and attributed operations
    Status { id: String },
    /// Read the entire recorded stdout, stderr, and exit code
    Result { id: String },
}

#[derive(Subcommand, Debug)]
pub enum OperationOp {
    /// List incomplete operations belonging to this workspace instance
    Pending,
    /// Show the durable state of one operation
    Status { id: String },
    /// Finish a prepared operation after checking every affected prestate
    Recover { id: String },
}

#[derive(alt_args::Args, Debug)]
pub struct PortableOptions {
    /// Private credential created by alt auth login
    #[arg(long)]
    pub credential_file: std::path::PathBuf,
    /// Private durable journal; reuse this directory after an interrupted attempt
    #[arg(long)]
    pub journal: std::path::PathBuf,
    #[arg(long, default_value_t = 16_777_216)]
    pub max_metadata_bytes: usize,
    #[arg(long, default_value_t = 1_000_000)]
    pub max_entries: usize,
    #[arg(long, default_value_t = 100_000)]
    pub max_publications: usize,
    #[arg(long, default_value_t = 8_589_934_592)]
    pub max_blob_bytes: u64,
    #[arg(long, default_value_t = 68_719_476_736)]
    pub max_total_bytes: u64,
    #[arg(long, default_value_t = 120)]
    pub timeout_seconds: u64,
}

#[derive(Subcommand, Debug)]
pub enum PortableOp {
    /// Download a complete signed snapshot without stopping source writes
    Export {
        #[command(flatten)]
        options: PortableOptions,
        #[arg(long)]
        repository: String,
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Inspect the same immutable source export
    ExportStatus {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Stop source writes only at the original export watermark
    FreezeSource {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Explicitly remove the original source write barrier
    ReopenSource {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Release retention for the original export; a frozen source must be reopened first
    RetireExport {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Verify a local archive and stage it privately in a new empty target repository
    Import {
        #[command(flatten)]
        options: PortableOptions,
        #[arg(long)]
        repository: String,
        #[arg(long)]
        archive: std::path::PathBuf,
        /// JSON list of explicit source identity dispositions; null links preserve history only
        #[arg(long)]
        mappings: std::path::PathBuf,
        #[arg(long)]
        source_installation: String,
        #[arg(long)]
        source_repository: String,
        #[arg(long)]
        source_epoch: u64,
        /// Source receipt public key obtained independently of the archive
        #[arg(long)]
        source_key: String,
    },
    /// Query the original target intent, including a lost activation response
    ImportStatus {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Atomically activate the original staged import under current target authority
    ActivateImport {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Abandon a receiving import without deleting the source or accepted history
    AbandonImport {
        #[command(flatten)]
        options: PortableOptions,
    },
}
