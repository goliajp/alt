//! The subcommands of `auth`, `runner`, `request`, `operation` and
//! `portable`.

use alt_args::Subcommand;

#[derive(Subcommand, Debug)]
pub enum AuthOp {
    /// Give an agent a short credential with explicit repository and write bounds
    Delegate {
        #[arg(long, hint = path)]
        credential_file: std::path::PathBuf,
        #[arg(long, hint = path)]
        agent_file: std::path::PathBuf,
        /// JSON purpose, agent_session, lifetime_seconds and scope
        #[arg(long, required_unless_present = "resume", conflicts_with = "resume", hint = path)]
        plan: Option<std::path::PathBuf>,
        /// Retry the same privately saved delegation after interruption
        #[arg(long)]
        resume: bool,
    },
    /// List agent credentials belonging to the current human account
    Delegations {
        #[arg(long, hint = path)]
        credential_file: std::path::PathBuf,
        #[arg(long, hint = none)]
        after: Option<String>,
    },
    /// Revoke one agent credential owned by the current human account
    RevokeAgent {
        #[arg(long, hint = path)]
        credential_file: std::path::PathBuf,
        #[arg(long, hint = none)]
        credential_id: String,
    },
    /// Save a private, origin-bound credential after explicit browser consent
    Login {
        #[arg(long, hint = none)]
        server: String,
        #[arg(long, hint = path)]
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
        #[arg(long, hint = path)]
        credential_file: std::path::PathBuf,
    },
    /// Revoke the saved credential before removing its private file
    Logout {
        #[arg(long, hint = path)]
        credential_file: std::path::PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub enum RunnerOp {
    Probe {
        #[arg(long, hint = none)]
        bindings: String,
        #[arg(long, hint = none)]
        environment: String,
    },
    Execute {
        #[arg(hint = path)]
        spec: std::path::PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub enum RequestOp {
    /// List requests without a durable completion
    Pending,
    /// Inspect request scope, completion, and attributed operations
    Status {
        #[arg(hint = none)]
        id: String,
    },
    /// Read the entire recorded stdout, stderr, and exit code
    Result {
        #[arg(hint = none)]
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum OperationOp {
    /// List incomplete operations belonging to this workspace instance
    Pending,
    /// Show the durable state of one operation
    Status {
        #[arg(hint = none)]
        id: String,
    },
    /// Finish a prepared operation after checking every affected prestate
    Recover {
        #[arg(hint = none)]
        id: String,
    },
    /// Give up the interrupted merge, cherry-pick, revert or rebase command an operation
    /// belongs to: the workspace is put back as it was before that command
    Abandon {
        #[arg(hint = none)]
        id: String,
        /// Leave a file that was changed since the command was interrupted as it is
        #[arg(long)]
        keep_changes: bool,
    },
}

#[derive(alt_args::Args, Debug)]
pub struct PortableOptions {
    /// Private credential created by alt auth login
    #[arg(long, hint = path)]
    pub credential_file: std::path::PathBuf,
    /// Private durable journal; reuse this directory after an interrupted attempt
    #[arg(long, hint = path)]
    pub journal: std::path::PathBuf,
    #[arg(long, default_value_t = 16_777_216, hint = none)]
    pub max_metadata_bytes: usize,
    #[arg(long, default_value_t = 1_000_000, hint = none)]
    pub max_entries: usize,
    #[arg(long, default_value_t = 100_000, hint = none)]
    pub max_publications: usize,
    #[arg(long, default_value_t = 8_589_934_592, hint = none)]
    pub max_blob_bytes: u64,
    #[arg(long, default_value_t = 68_719_476_736, hint = none)]
    pub max_total_bytes: u64,
    #[arg(long, default_value_t = 120, hint = none)]
    pub timeout_seconds: u64,
}

#[derive(Subcommand, Debug)]
pub enum PortableOp {
    /// Download a complete signed snapshot without stopping source writes
    Export {
        #[command(flatten)]
        options: PortableOptions,
        #[arg(long, hint = none)]
        repository: String,
        #[arg(long, hint = path)]
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
        #[arg(long, hint = none)]
        repository: String,
        #[arg(long, hint = path)]
        archive: std::path::PathBuf,
        /// JSON list of explicit source identity dispositions; null links preserve history only
        #[arg(long, hint = path)]
        mappings: std::path::PathBuf,
        #[arg(long, hint = none)]
        source_installation: String,
        #[arg(long, hint = none)]
        source_repository: String,
        #[arg(long, hint = none)]
        source_epoch: u64,
        /// Source receipt public key obtained independently of the archive
        #[arg(long, hint = none)]
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
