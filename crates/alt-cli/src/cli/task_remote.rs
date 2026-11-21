//! The subcommands of `task remote`.

use alt_args::Subcommand;

#[derive(Subcommand, Debug)]
pub enum ChangeOp {
    /// List the explicitly configured Sentori channels
    Channels {
        #[arg(hint = none)]
        remote: String,
    },
    /// List historical facts and their current delivery state
    List {
        #[arg(hint = none)]
        remote: String,
        #[arg(long, hint = none)]
        after: Option<String>,
    },
    /// Read an original signed fact under current repository permission
    Show {
        #[arg(hint = none)]
        remote: String,
        #[arg(hint = none)]
        digest: String,
    },
    /// Inspect a repair request and any actually published native Task
    Repair {
        #[arg(hint = none)]
        remote: String,
        #[arg(hint = none)]
        digest: String,
    },
    /// Create a local Task from a currently authorized repair request
    RepairCreate {
        #[arg(hint = none)]
        remote: String,
        #[arg(hint = none)]
        digest: String,
        /// Normal TaskSpec JSON; source is supplied by the verified request
        #[arg(long, hint = none)]
        definition: String,
        #[arg(long, hint = none)]
        target: String,
        /// Explicitly replace an interrupted local creation under a new request
        #[arg(long, hint = none)]
        reconcile_request: Option<String>,
    },
    /// Save an exact Delivery mapping or withdrawal before sending it
    Preview {
        #[arg(hint = none)]
        remote: String,
        #[arg(long, hint = path)]
        plan: std::path::PathBuf,
    },
    /// Send or resume a saved association using its original request
    Send {
        #[arg(hint = none)]
        id: String,
    },
    /// Query a saved association without creating a new request
    Status {
        #[arg(hint = none)]
        id: String,
    },
    /// Explicitly resume a pending statement with this actor's current credential
    Reauthorize {
        #[arg(hint = none)]
        remote: String,
        #[arg(hint = none)]
        channel: String,
        #[arg(hint = none)]
        digest: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TaskRemoteOp {
    /// Inspect and publish exact associations with Sentori
    Changes {
        #[command(subcommand)]
        op: ChangeOp,
    },
    /// Bind an HTTPS origin, repository and operator-provided receipt key
    Connect {
        #[arg(hint = none)]
        name: String,
        #[arg(long, hint = none)]
        url: String,
        #[arg(long, hint = none)]
        repository: String,
        #[arg(long, hint = none)]
        receipt_key: String,
        #[arg(long)]
        allow_loopback_http: bool,
    },
    /// Freeze the exact records, attachments and optional ref updates to publish
    Preview {
        #[arg(hint = none)]
        remote: String,
        /// JSON selection and optional ref updates; only these roots are followed
        #[arg(long, hint = path)]
        plan: std::path::PathBuf,
    },
    /// Send or resume an already previewed publication using its original ID
    Publish {
        #[arg(hint = none)]
        id: String,
        #[arg(long)]
        prepare_only: bool,
    },
    /// Sign the original completed evidence statement with a collector key
    Attest {
        #[arg(hint = none)]
        evidence: String,
        #[arg(long, hint = path)]
        key_file: std::path::PathBuf,
    },
    /// Download the complete ready selection for an independent current review
    Review {
        #[arg(hint = none)]
        remote: String,
        #[arg(long, hint = none)]
        actor: String,
        #[arg(long, hint = none)]
        request: String,
    },
    /// Approve the exact previously inspected request as the current reviewer
    Approve {
        #[arg(hint = none)]
        remote: String,
        #[arg(long, hint = none)]
        actor: String,
        #[arg(long, hint = none)]
        request: String,
        #[arg(long, hint = none)]
        integration: String,
        #[arg(long, hint = none)]
        digest: String,
    },
    /// Query the authoritative outcome without sending a new publication
    Status {
        #[arg(hint = none)]
        id: String,
    },
    /// List published roots, or the signed publications containing a root
    List {
        #[arg(hint = none)]
        remote: String,
        #[arg(long, hint = none)]
        kind: String,
        #[arg(long, hint = none)]
        id: Option<String>,
        #[arg(long, hint = none)]
        after: Option<String>,
    },
    /// Receive an explicitly selected signed publication; never execute it
    Receive {
        #[arg(hint = none)]
        remote: String,
        #[arg(long, hint = none)]
        kind: String,
        #[arg(long, hint = none)]
        id: String,
        #[arg(long, hint = none)]
        publication: String,
    },
    /// Accept a received task in this isolated workspace with its original ID
    Adopt {
        #[arg(hint = none)]
        task: String,
        #[arg(long, hint = none)]
        publication: String,
        /// Explicitly reauthorize an interrupted adoption under a new request
        #[arg(long, hint = none)]
        reconcile_request: Option<String>,
    },
}
