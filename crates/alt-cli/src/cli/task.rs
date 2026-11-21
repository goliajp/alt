//! The subcommands of `task`, `candidate` and `evidence`.

use alt_args::Subcommand;

#[derive(Subcommand, Debug)]
pub enum TaskOp {
    /// Publish selected task history or receive and continue published work
    Remote {
        #[command(subcommand)]
        op: TaskRemoteOp,
    },
    /// Show the complete candidate diff against the task baseline
    Changes {
        candidate: String,
    },
    /// Record explicit review of a current candidate and its observed evidence
    Review {
        candidate: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        reason: String,
        #[arg(long, short = 'm')]
        message: String,
    },
    ReviewShow {
        id: String,
    },
    /// Atomically integrate a reviewed candidate into its unoccupied target
    Integrate {
        review: String,
    },
    IntegrationShow {
        id: String,
    },
    /// Collect an already recorded ref transaction without applying it again
    IntegrationCollect {
        id: String,
    },
    /// Export a completed integration to a verified independent Git directory
    Export {
        integration: String,
        target: std::path::PathBuf,
        #[arg(long, default_value = "/usr/bin/git")]
        git: std::path::PathBuf,
    },
    DeliveryShow {
        id: String,
    },
    /// Verify an existing delivery after a lost publication response
    DeliveryCollect {
        id: String,
        #[arg(long, default_value = "/usr/bin/git")]
        git: std::path::PathBuf,
    },
    /// Record a task definition and current isolated-workspace baseline
    Create {
        /// JSON TaskSpec; pass a file's contents with shell substitution
        #[arg(long)]
        definition: String,
        /// Existing integration target, recorded with its expected object id
        #[arg(long)]
        target: String,
    },
    List,
    Show {
        id: String,
    },
    /// Append an abandonment record; keep candidates, logs and workspace files
    Abandon {
        id: String,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ChangeOp {
    /// List the explicitly configured Sentori channels
    Channels { remote: String },
    /// List historical facts and their current delivery state
    List {
        remote: String,
        #[arg(long)]
        after: Option<String>,
    },
    /// Read an original signed fact under current repository permission
    Show { remote: String, digest: String },
    /// Inspect a repair request and any actually published native Task
    Repair { remote: String, digest: String },
    /// Create a local Task from a currently authorized repair request
    RepairCreate {
        remote: String,
        digest: String,
        /// Normal TaskSpec JSON; source is supplied by the verified request
        #[arg(long)]
        definition: String,
        #[arg(long)]
        target: String,
        /// Explicitly replace an interrupted local creation under a new request
        #[arg(long)]
        reconcile_request: Option<String>,
    },
    /// Save an exact Delivery mapping or withdrawal before sending it
    Preview {
        remote: String,
        #[arg(long)]
        plan: std::path::PathBuf,
    },
    /// Send or resume a saved association using its original request
    Send { id: String },
    /// Query a saved association without creating a new request
    Status { id: String },
    /// Explicitly resume a pending statement with this actor's current credential
    Reauthorize {
        remote: String,
        channel: String,
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
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        repository: String,
        #[arg(long)]
        receipt_key: String,
        #[arg(long)]
        allow_loopback_http: bool,
    },
    /// Freeze the exact records, attachments and optional ref updates to publish
    Preview {
        remote: String,
        /// JSON selection and optional ref updates; only these roots are followed
        #[arg(long)]
        plan: std::path::PathBuf,
    },
    /// Send or resume an already previewed publication using its original ID
    Publish {
        id: String,
        #[arg(long)]
        prepare_only: bool,
    },
    /// Sign the original completed evidence statement with a collector key
    Attest {
        evidence: String,
        #[arg(long)]
        key_file: std::path::PathBuf,
    },
    /// Download the complete ready selection for an independent current review
    Review {
        remote: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        request: String,
    },
    /// Approve the exact previously inspected request as the current reviewer
    Approve {
        remote: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        request: String,
        #[arg(long)]
        integration: String,
        #[arg(long)]
        digest: String,
    },
    /// Query the authoritative outcome without sending a new publication
    Status { id: String },
    /// List published roots, or the signed publications containing a root
    List {
        remote: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        after: Option<String>,
    },
    /// Receive an explicitly selected signed publication; never execute it
    Receive {
        remote: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        publication: String,
    },
    /// Accept a received task in this isolated workspace with its original ID
    Adopt {
        task: String,
        #[arg(long)]
        publication: String,
        /// Explicitly reauthorize an interrupted adoption under a new request
        #[arg(long)]
        reconcile_request: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum CandidateOp {
    /// Snapshot index-selected source plus the task's explicit auxiliary roots
    Create {
        task: String,
        #[arg(long)]
        parent: Option<String>,
    },
    Show {
        id: String,
    },
    /// Recompute current source and auxiliary inputs without accepting old evidence
    Check {
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum EvidenceOp {
    /// Materialize the candidate and execute through the selected runner
    Run {
        candidate: String,
        #[arg(long)]
        profile: String,
    },
    /// Read an existing run report without launching validation again
    Collect {
        id: String,
    },
    /// Inspect durable launch attribution and collection receipts
    RunStatus {
        id: String,
    },
    /// Preserve an external report; imported claims never become observed runs
    Import {
        candidate: String,
        #[arg(long)]
        report: String,
        #[arg(long)]
        stdout: Option<std::path::PathBuf>,
        #[arg(long)]
        stderr: Option<std::path::PathBuf>,
    },
    Show {
        id: String,
    },
    Check {
        id: String,
    },
    /// Read an entire preserved output stream without truncation
    Log {
        id: String,
        #[arg(value_parser = ["stdout", "stderr", "original-report"])]
        stream: String,
    },
}
