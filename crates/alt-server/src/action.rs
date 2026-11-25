//! What an operator asks of an instance on the command line.

use alt_args::{Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Subcommand)]
pub(crate) enum Action {
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
        /// Back up an instance whose pack segments hold objects that are not
        /// read, and name those segments in the manifest
        #[arg(long)]
        with_unread: bool,
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
        /// Restore pack segments that hold objects that are not read, though
        /// the backup does not name them
        #[arg(long)]
        with_unread: bool,
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
        #[arg(long, action = set)]
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
    /// List the trees a push is now refused for, of one repository or all; changes nothing
    RefusedTrees {
        repository: Option<String>,
    },
    /// Read every object of the live pack segments; one repository, or all
    Scrub {
        repository: Option<String>,
        /// Replace a segment that does not give its objects back by its pack indexed anew
        #[arg(long)]
        mend: bool,
    },
}
#[derive(Clone, ValueEnum)]
pub(crate) enum SecretKind {
    ReceiptKey,
    Token,
}
