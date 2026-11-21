//! The top-level commands.

use alt_args::Subcommand;

use super::vcs::parse_key_value;
use super::*;
use crate::{log_cmd, worktree_args};

#[derive(Subcommand)]
pub enum Command {
    /// Transfer signed repository snapshots with explicit cutover and durable request recovery
    Portable {
        #[command(subcommand)]
        op: PortableOp,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Sign in to a server through explicit browser device consent
    Auth {
        #[command(subcommand)]
        op: AuthOp,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Inspect tools or execute one immutable validation snapshot
    Runner {
        #[command(subcommand)]
        op: RunnerOp,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Record an authorized task in an isolated workspace
    Task {
        #[command(subcommand)]
        op: TaskOp,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Capture and inspect immutable candidates and complete input identities
    Candidate {
        #[command(subcommand)]
        op: CandidateOp,
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
    /// Inspect durable whole-command receipts without executing the command
    Request {
        #[command(subcommand)]
        op: RequestOp,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Provide contents or details of repository objects
    CatFile(CatFileArgs),
    /// Pick out and massage parameters
    RevParse { rev: String },
    /// Show commit logs
    Log(log_cmd::LogArgs),
    /// Verify and independently import a supported Git source for tasks
    Intake {
        source: std::path::PathBuf,
        /// New directory outside every repository (must not already exist)
        target: std::path::PathBuf,
        /// Absolute Git executable used for independent verification
        #[arg(long, default_value = "/usr/bin/git")]
        git: std::path::PathBuf,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Import this git repository into a .alt store
    Import {
        /// Destination directory: the store is created at <DIR>/.alt
        /// (keep it outside any git work tree — .alt does not coexist
        /// with .git)
        target: std::path::PathBuf,
    },
    /// Export this .alt store back to a git repository
    Export {
        /// Destination directory (must not exist or be empty): a .git is
        /// rebuilt at <DIR>/.git with L1 semantic fidelity
        target: std::path::PathBuf,
    },
    /// Create an empty native .alt repository
    Init {
        /// Directory to create the repo in (default: current directory)
        dir: Option<std::path::PathBuf>,
    },
    /// Stage file contents into the index
    Add {
        #[command(flatten)]
        args: worktree_args::Add,
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
        #[command(flatten)]
        args: worktree_args::Status,
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
    /// Switch branches, materializing the target tree into the work tree
    Switch {
        /// Branch to switch to
        name: String,
        /// Create the branch before switching
        #[arg(short = 'c')]
        create: bool,
        /// Emit a structured JSON result instead of the human line
        #[arg(long)]
        json: bool,
    },
    /// Show a commit: header + per-file unified diff against its first parent
    Show {
        /// Revision to show (default `HEAD`). Accepts any spec `rev-parse` resolves.
        rev: Option<String>,
        /// Emit a structured JSON diff instead of unified text
        #[arg(long)]
        json: bool,
    },
    /// Line-by-line origin attribution for one file (`git blame`-style)
    Blame {
        /// Path to the file to blame (relative to the current directory).
        path: String,
        /// Revision to start the walk from (default `HEAD`).
        rev: Option<String>,
        /// Trace lines moved within the file. A number against the letter
        /// is how many letters and digits they must carry (20 if absent).
        #[arg(short = 'M', action = attached)]
        moves: Vec<String>,
        /// Accepted for command lines written before renames were always
        /// followed; it changes nothing.
        #[arg(long = "follow")]
        follow: bool,
        /// Trace lines copied from files changed in the same commit; twice,
        /// from any file when the blamed file is created; three times, from
        /// any file in every commit. Implies -M. A number against the
        /// letter works as for -M (40 if absent).
        #[arg(short = 'C', action = attached)]
        copies: Vec<String>,
    },
    /// Show or change the metadata recorded with a commit (kept in git
    /// notes under refs/notes/alt/meta; the commit itself never changes)
    Meta {
        #[command(subcommand)]
        op: MetaOp,
    },
    /// Git LFS: the large contents behind the pointer files LFS paths commit
    Lfs {
        #[command(subcommand)]
        op: LfsOp,
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
    /// Apply the changes from a commit on top of the current branch
    CherryPick {
        /// Revision of the commit to apply
        rev: String,
        /// Emit a structured JSON result instead of human lines.
        #[arg(long)]
        json: bool,
    },
    /// Read or update git-style config keys (`user.name`, …)
    Config {
        /// Dotted key (e.g. `user.name`). Omit when `--list` or
        /// `--unset` is set.
        key: Option<String>,
        /// New value. When supplied, the key is set (overwriting any
        /// existing value); when omitted, the key is read.
        value: Option<String>,
        /// List every entry as `section.key=value` (git's default
        /// `--list` shape).
        #[arg(long)]
        list: bool,
        /// Remove a key from the config file.
        #[arg(long)]
        unset: Option<String>,
    },
    /// List, create (lightweight or annotated), or delete tags
    Tag {
        /// Tag name to create. Omit (and omit `-d`) to list all tags.
        name: Option<String>,
        /// Revision the new tag should point to (default HEAD). Only
        /// honoured when creating a tag.
        rev: Option<String>,
        /// Delete a tag by name. Mutually exclusive with `name`.
        #[arg(short = 'd', long)]
        delete: Option<String>,
        /// Create an *annotated* tag (writes a tag object carrying the
        /// message + tagger ident, instead of a bare ref).
        #[arg(short = 'a', long)]
        annotate: bool,
        /// Annotated-tag message. Required when `-a` is set.
        #[arg(short = 'm', long)]
        message: Option<String>,
        /// JSON output for list mode (ignored otherwise).
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
    /// Join another branch into the current one
    Merge {
        /// Branch to merge into the current branch
        branch: String,
        /// Emit a structured JSON result instead of the human lines
        #[arg(long)]
        json: bool,
    },
    /// git-flow workflow operations (atomic, undoable)
    Flow {
        #[command(subcommand)]
        op: FlowOp,
        /// Emit a structured JSON result instead of the human line
        #[arg(long, global = true)]
        json: bool,
    },
    /// Undo the last branch/HEAD operation (inverts one op log entry)
    Undo {
        /// Emit a structured JSON result instead of the human line
        #[arg(long)]
        json: bool,
    },
    /// Inspect and finish interrupted local operations
    Operation {
        #[command(subcommand)]
        op: OperationOp,
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
    /// Manage git remotes (persisted as `<alt-dir>/remotes/<name>`)
    Remote {
        #[command(subcommand)]
        op: RemoteOp,
        /// Emit a structured JSON result instead of the human view
        #[arg(long, global = true)]
        json: bool,
    },
    /// Clone a remote repository: init + remote add origin + fetch + checkout
    Clone {
        /// Remote URL (e.g. `https://github.com/user/repo.git`)
        url: String,
        /// Destination directory (defaults to the URL's last path
        /// segment with any `.git` suffix stripped)
        dir: Option<std::path::PathBuf>,
        /// Emit a structured JSON result instead of the human view
        #[arg(long)]
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
    /// Push refs + objects to a configured remote (git smart-http v1)
    Push {
        /// Remote name (defaults to `origin`)
        #[arg(default_value = "origin")]
        remote: String,
        /// Refspecs to push (`<src>:<dst>` or just `<src>` — defaults to
        /// the current branch onto its same-named remote branch)
        refspecs: Vec<String>,
        /// Force the update (allow non-fast-forward); the server still
        /// enforces its own policy
        #[arg(short = 'f', long)]
        force: bool,
        /// Emit a structured JSON result instead of the human view
        #[arg(long)]
        json: bool,
    },
    /// Manage local identities + trusted public keys
    Identity {
        #[command(subcommand)]
        op: IdentityOp,
        /// Emit a structured JSON result instead of the human view
        #[arg(long, global = true)]
        json: bool,
    },
    /// Audit-view the op log: who did what, in order, with the parsed
    /// principal and any ref changes carried in each op's payload.
    #[command(name = "op-log")]
    OpLog {
        /// Limit the number of entries shown (newest first; default = all)
        #[arg(short = 'n')]
        max_count: Option<usize>,
        /// Emit a structured JSON list instead of the human view
        #[arg(long)]
        json: bool,
        /// Check each op against the signature sidecar + trust store;
        /// rows tagged `signed-ok` / `unsigned` / `bad-sig` /
        /// `untrusted`
        #[arg(long)]
        verify: bool,
    },
    /// alt-CI workflow inspection. Subcommand
    /// surface; `alt ci validate` runs the schema-tier lint.
    Ci {
        #[command(subcommand)]
        op: CiOp,
        /// Emit line-delimited JSON diagnostics instead of the human
        /// `<path>:<line>:<col>: <severity>: <message>` form
        #[arg(long, global = true)]
        json: bool,
    },
    /// Read and verify every record the repository stores: each chunk,
    /// blob and object against its hash, each index against the file it
    /// indexes, the whole operation log, and the refs against a replay of
    /// that log. Exits non-zero at the first record that fails.
    Fsck {
        /// Emit the record counts as a JSON object
        #[arg(long)]
        json: bool,
    },
    /// Verify the `alt-sig` header on one or more commit objects.
    /// With no args, walks the current branch's commit chain back to root
    /// (newest first). Each row is one of:
    /// `signed-ok:<principal>` / `unsigned` / `bad-sig:<principal>` /
    /// `untrusted:<principal>`.
    Verify {
        /// Commit oids to check (hex). Empty = walk HEAD.
        commits: Vec<String>,
        /// Limit when walking HEAD (default 50; ignored when oids given)
        #[arg(short = 'n', long = "max-count")]
        max_count: Option<usize>,
        /// Emit a structured JSON list instead of the human view
        #[arg(long)]
        json: bool,
    },
}
