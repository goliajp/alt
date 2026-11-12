//! A copy of part of a real command line, so that the outputs recorded
//! in `fixtures/alt.txt` apply to it word for word.
#![allow(dead_code)]

use alt_args::{Args, Parser, Subcommand};

use crate::portable::PortableOp;

#[derive(Parser)]
#[command(name = "alt", version, disable_help_subcommand = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
    /// Operate in the named parallel workspace instead of the default one
    #[arg(long, global = true)]
    pub workspace: Option<String>,
    /// Stable request identity for a write in an existing native repository
    #[arg(long, global = true, value_name = "HEX")]
    pub request_id: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Transfer signed repository snapshots with explicit cutover and durable request recovery
    Portable {
        #[command(subcommand)]
        op: PortableOp,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Preserve validation reports and inspect their current validity
    Evidence {
        #[command(subcommand)]
        op: EvidenceOp,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Provide contents or details of repository objects
    CatFile(CatFileArgs),
    /// Show commit logs
    Log(LogArgs),
    /// Stage file contents into the index
    Add {
        /// Paths to stage; `.` stages the whole working tree
        paths: Vec<String>,
        /// Emit a structured JSON result instead of the human line
        #[arg(long)]
        json: bool,
    },
    /// Record staged changes as a new commit
    Commit {
        /// Commit message (with --amend, defaults to the amended commit's)
        #[arg(short = 'm', required_unless_present_any = ["amend", "validate"])]
        message: Option<String>,
        /// Replace the current branch tip with a new commit made from the
        /// index, keeping its parents and author
        #[arg(long)]
        amend: bool,
        /// Keep the amended commit's message (alt never opens an editor, so
        /// this is already the default without -m; accepted as in git)
        #[arg(long, requires = "amend")]
        no_edit: bool,
        /// Record the commit even when it changes nothing
        #[arg(long)]
        allow_empty: bool,
        /// Skip the pre-commit checks (secret scan, large-file warning)
        #[arg(short = 'n', long)]
        no_verify: bool,
        /// Only run the pre-commit checks on what is staged; commit nothing
        #[arg(long, conflicts_with_all = ["amend", "message", "no_verify"])]
        validate: bool,
        /// Record a one-line decision in the commit's metadata (up to 3)
        #[arg(long = "decision", value_name = "TEXT")]
        decisions: Vec<String>,
        /// Record an extra metadata field (`key=value`, e.g. `model=…`)
        #[arg(long = "meta", value_name = "KEY=VALUE", value_parser = parse_key_value)]
        extra: Vec<(String, String)>,
        /// Emit the new commit/tree oids (or, with --validate, the findings)
        /// as a JSON object
        #[arg(long)]
        json: bool,
    },
    /// Show the working tree status
    Status {
        /// Emit a stable JSON object instead of the human-readable view
        #[arg(long)]
        json: bool,
    },
    /// List, create, or delete branches
    Branch {
        /// Branch name to create (omit to list branches)
        name: Option<String>,
        /// Delete the named branch
        #[arg(short = 'd')]
        delete: Option<String>,
        /// With -d, the branch is a remote-tracking one (`<remote>/<branch>`)
        #[arg(short = 'r', long = "remotes", requires = "delete")]
        remotes: bool,
        /// Emit the branch list as a stable JSON object
        #[arg(long)]
        json: bool,
    },
    /// Find the commit that introduced a change by binary search
    Bisect {
        #[command(subcommand)]
        op: BisectOp,
    },
    /// Replay the current branch's own commits onto another base
    Rebase {
        /// The base to replay onto
        #[arg(required_unless_present_any = ["cont", "skip", "abort"])]
        upstream: Option<String>,
        /// Edit the list of commits first (pick, reword, squash, fixup,
        /// drop) in $GIT_SEQUENCE_EDITOR, $GIT_EDITOR, $VISUAL or $EDITOR
        #[arg(short = 'i', long)]
        interactive: bool,
        /// Commit the resolved step and go on
        #[arg(long = "continue", id = "cont", conflicts_with_all = ["upstream", "skip", "abort"])]
        cont: bool,
        /// Drop the stopped step and go on
        #[arg(long, conflicts_with_all = ["upstream", "abort"])]
        skip: bool,
        /// Put the branch back as it was before the rebase
        #[arg(long, conflicts_with = "upstream")]
        abort: bool,
        /// Emit a structured JSON result instead of human lines.
        #[arg(long)]
        json: bool,
    },
    /// Record new commits that undo earlier ones
    Revert {
        /// Commits to revert, in order
        #[arg(required_unless_present_any = ["cont", "abort"])]
        revs: Vec<String>,
        /// For a merge commit, the parent (1-based) whose side is kept
        #[arg(short = 'm', long = "mainline")]
        mainline: Option<usize>,
        /// Commit the resolved revert and go on with the rest
        #[arg(long = "continue", id = "cont", conflicts_with_all = ["revs", "abort", "mainline"])]
        cont: bool,
        /// Put the branch back where it was before the reverts
        #[arg(long, conflicts_with_all = ["revs", "mainline"])]
        abort: bool,
        /// Use the generated message (alt never opens an editor; accepted as in git)
        #[arg(long)]
        no_edit: bool,
        /// Emit a structured JSON result instead of human lines.
        #[arg(long)]
        json: bool,
    },
    /// Show changes between the index and the working tree (or HEAD)
    Diff {
        /// Show staged changes (HEAD vs index) instead of unstaged
        #[arg(long, visible_alias = "staged")]
        cached: bool,
        /// Emit a structured JSON diff instead of unified text
        #[arg(long)]
        json: bool,
        /// Show an AST-level diff per supported language: item-level
        /// logical vs format-only changes for `.rs` files; line/binary diff
        /// for files without a parser.
        #[arg(long)]
        semantic: bool,
    },
    /// git-flow workflow operations (atomic, undoable)
    Flow {
        #[command(subcommand)]
        op: FlowOp,
        /// Emit a structured JSON result instead of the human line
        #[arg(long, global = true)]
        json: bool,
    },
    /// Manage parallel workspaces (isolated HEAD/index/working tree)
    Workspace {
        #[command(subcommand)]
        op: WorkspaceOp,
        /// Emit a structured JSON result instead of the human view
        #[arg(long, global = true)]
        json: bool,
    },
    /// Fetch refs + objects from a configured remote (git smart-http v2)
    Fetch {
        /// Remote name (defaults to `origin`)
        #[arg(default_value = "origin")]
        remote: String,
        /// Refspecs to fetch (defaults to the remote's configured refspec)
        refspecs: Vec<String>,
        /// Emit a structured JSON result instead of the human view
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum BisectOp {
    /// Start a session, optionally naming the bad commit and good ones
    Start {
        /// The bad commit
        bad: Option<String>,
        /// Known good commits
        good: Vec<String>,
    },
    /// Mark commits (default: the one being tested) as bad
    Bad { revs: Vec<String> },
    /// Mark commits (default: the one being tested) as good
    Good { revs: Vec<String> },
    /// Mark commits (default: the one being tested) as untestable
    Skip { revs: Vec<String> },
    /// End the session and go back to the branch
    Reset,
    /// Let a command judge each commit: exit 0 good, 125 skip, 1-127 bad,
    /// anything else stops
    Run {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum WorkspaceOp {
    /// Create a workspace with its own working tree, checked out on a branch
    Add {
        /// Workspace name
        name: String,
        /// Working-tree directory for the workspace
        path: std::path::PathBuf,
        /// Branch to check out (defaults to a new branch named after the
        /// workspace, at the current commit)
        branch: Option<String>,
    },
    /// List the workspaces
    List,
    /// Remove a workspace (its HEAD ref and control dir; files are kept)
    Remove {
        /// Workspace name
        name: String,
    },
}

#[derive(Subcommand)]
pub enum FlowOp {
    /// Create the `develop` branch off `main` and switch to it
    Init,
    /// Feature-branch operations
    Feature {
        #[command(subcommand)]
        op: FlowTopicOp,
    },
    /// Release-branch operations
    Release {
        #[command(subcommand)]
        op: FlowTopicOp,
    },
    /// Hotfix-branch operations
    Hotfix {
        #[command(subcommand)]
        op: FlowTopicOp,
    },
}

#[derive(Subcommand)]
pub enum FlowTopicOp {
    /// Branch the topic off its base and switch to it
    Start { name: String },
    /// Merge the topic back into its target(s) and delete it (atomic
    /// ref transaction + single op-log entry, like every other flow op)
    Finish { name: String },
}

#[derive(Subcommand)]
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

pub fn parse_key_value(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .ok_or_else(|| format!("expected key=value, got '{s}'"))
}

#[derive(Args)]
pub struct CatFileArgs {
    /// show object type
    #[arg(short = 't', group = "op")]
    pub show_type: bool,
    /// show object size
    #[arg(short = 's', group = "op")]
    pub show_size: bool,
    /// pretty-print object's content
    #[arg(short = 'p', group = "op")]
    pub pretty: bool,
    pub object: String,
}

#[derive(Args)]
pub struct LogArgs {
    /// pretty-print commits: raw | oneline (ignored with --json)
    #[arg(long, default_value = "oneline")]
    pub pretty: String,
    /// limit the number of commits
    #[arg(short = 'n')]
    pub max_count: Option<usize>,
    /// emit the commit list as a stable JSON object
    #[arg(long)]
    pub json: bool,
    /// show a per-commit patch (unified diff for text, compact chunk +
    /// perceptual summary for binary so large-file history doesn't blow up
    /// the terminal). Ignored with --json.
    #[arg(short = 'p', long = "patch")]
    pub patch: bool,
    /// start revision
    #[arg(default_value = "HEAD")]
    pub rev: String,
}
