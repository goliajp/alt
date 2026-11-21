//! The subcommands of `task`, `candidate` and `evidence`.

use alt_args::Subcommand;

use super::task_remote::TaskRemoteOp;

#[derive(Subcommand, Debug)]
pub enum TaskOp {
    /// Publish selected task history or receive and continue published work
    Remote {
        #[command(subcommand)]
        op: TaskRemoteOp,
    },
    /// Show the complete candidate diff against the task baseline
    Changes {
        #[arg(hint = none)]
        candidate: String,
    },
    /// Record explicit review of a current candidate and its observed evidence
    Review {
        #[arg(hint = none)]
        candidate: String,
        #[arg(long, hint = none)]
        evidence: String,
        #[arg(long, hint = none)]
        reviewer: String,
        #[arg(long, hint = none)]
        reason: String,
        #[arg(long, short = 'm', hint = none)]
        message: String,
    },
    ReviewShow {
        #[arg(hint = none)]
        id: String,
    },
    /// Atomically integrate a reviewed candidate into its unoccupied target
    Integrate {
        #[arg(hint = none)]
        review: String,
    },
    IntegrationShow {
        #[arg(hint = none)]
        id: String,
    },
    /// Collect an already recorded ref transaction without applying it again
    IntegrationCollect {
        #[arg(hint = none)]
        id: String,
    },
    /// Export a completed integration to a verified independent Git directory
    Export {
        #[arg(hint = none)]
        integration: String,
        #[arg(hint = path)]
        target: std::path::PathBuf,
        #[arg(long, default_value = "/usr/bin/git", hint = path)]
        git: std::path::PathBuf,
    },
    DeliveryShow {
        #[arg(hint = none)]
        id: String,
    },
    /// Verify an existing delivery after a lost publication response
    DeliveryCollect {
        #[arg(hint = none)]
        id: String,
        #[arg(long, default_value = "/usr/bin/git", hint = path)]
        git: std::path::PathBuf,
    },
    /// Record a task definition and current isolated-workspace baseline
    Create {
        /// JSON TaskSpec; pass a file's contents with shell substitution
        #[arg(long, hint = none)]
        definition: String,
        /// Existing integration target, recorded with its expected object id
        #[arg(long, hint = none)]
        target: String,
    },
    List,
    Show {
        #[arg(hint = none)]
        id: String,
    },
    /// Append an abandonment record; keep candidates, logs and workspace files
    Abandon {
        #[arg(hint = none)]
        id: String,
        #[arg(long, hint = none)]
        reason: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum CandidateOp {
    /// Snapshot index-selected source plus the task's explicit auxiliary roots
    Create {
        #[arg(hint = none)]
        task: String,
        #[arg(long, hint = none)]
        parent: Option<String>,
    },
    Show {
        #[arg(hint = none)]
        id: String,
    },
    /// Recompute current source and auxiliary inputs without accepting old evidence
    Check {
        #[arg(hint = none)]
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum EvidenceOp {
    /// Materialize the candidate and execute through the selected runner
    Run {
        #[arg(hint = none)]
        candidate: String,
        #[arg(long, hint = none)]
        profile: String,
    },
    /// Read an existing run report without launching validation again
    Collect {
        #[arg(hint = none)]
        id: String,
    },
    /// Inspect durable launch attribution and collection receipts
    RunStatus {
        #[arg(hint = none)]
        id: String,
    },
    /// Preserve an external report; imported claims never become observed runs
    Import {
        #[arg(hint = none)]
        candidate: String,
        #[arg(long, hint = none)]
        report: String,
        #[arg(long, hint = path)]
        stdout: Option<std::path::PathBuf>,
        #[arg(long, hint = path)]
        stderr: Option<std::path::PathBuf>,
    },
    Show {
        #[arg(hint = none)]
        id: String,
    },
    Check {
        #[arg(hint = none)]
        id: String,
    },
    /// Read an entire preserved output stream without truncation
    Log {
        #[arg(hint = none)]
        id: String,
        #[arg(value_parser = ["stdout", "stderr", "original-report"], hint = none)]
        stream: String,
    },
}
