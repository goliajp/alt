//! The subcommands of the version-control commands.

use alt_args::Subcommand;

/// `alt meta` operations.
#[derive(Subcommand)]
pub enum MetaOp {
    /// Print a commit's metadata
    Show {
        /// The commit (default: HEAD)
        #[arg(default_value = "HEAD", hint = "rev")]
        rev: String,
        /// Emit a JSON object instead of the note text
        #[arg(long)]
        json: bool,
    },
    /// Replace a commit's message or add to its metadata, leaving the
    /// commit itself untouched
    Set {
        /// The commit (default: HEAD)
        #[arg(default_value = "HEAD", hint = "rev")]
        rev: String,
        /// A message that stands in for the commit's own in `alt log`
        #[arg(short = 'm', value_name = "MESSAGE", hint = none)]
        message: Option<String>,
        /// Add a one-line decision (up to 3 in total)
        #[arg(long = "decision", value_name = "TEXT", hint = none)]
        decisions: Vec<String>,
        /// Set an extra field (`key=value`)
        #[arg(long = "meta", value_name = "KEY=VALUE", value_parser = parse_key_value, hint = none)]
        extra: Vec<(String, String)>,
    },
}

pub(super) use crate::commit_args::parse_key_value;

/// `alt lfs` operations.
#[derive(Subcommand)]
pub enum LfsOp {
    /// Download the LFS contents HEAD needs from a remote's LFS server and
    /// check them out
    Fetch {
        #[arg(default_value = "origin", hint = "remote")]
        remote: String,
    },
    /// Take the LFS contents HEAD needs from a git repository's local LFS
    /// storage (as after `alt import` of a git-lfs repository)
    Import {
        /// The git directory (`.git`) holding `lfs/objects`
        #[arg(hint = dir)]
        git_dir: std::path::PathBuf,
    },
    /// List HEAD's LFS files: `*` when the content is here, `-` when not
    Ls,
}

/// `alt bisect` steps, as in git.
#[derive(Subcommand)]
pub enum BisectOp {
    /// Start a session, optionally naming the bad commit and good ones
    Start {
        /// The bad commit
        #[arg(hint = "rev")]
        bad: Option<String>,
        /// Known good commits
        #[arg(hint = "rev")]
        good: Vec<String>,
    },
    /// Mark commits (default: the one being tested) as bad
    Bad {
        #[arg(hint = "rev")]
        revs: Vec<String>,
    },
    /// Mark commits (default: the one being tested) as good
    Good {
        #[arg(hint = "rev")]
        revs: Vec<String>,
    },
    /// Mark commits (default: the one being tested) as untestable
    Skip {
        #[arg(hint = "rev")]
        revs: Vec<String>,
    },
    /// End the session and go back to the branch
    Reset,
    /// Let a command judge each commit: exit 0 good, 125 skip, 1-127 bad,
    /// anything else stops
    Run {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, hint = none)]
        cmd: Vec<String>,
    },
}

/// `alt ci` subcommand surface. Only `validate` exists so far; future
/// segments (runner / scheduler / cache) land here as new variants.
#[derive(Subcommand)]
pub enum CiOp {
    /// Validate one or more `workflow.toml` files against the alt-CI
    /// schema. With no arguments, scans `.alt/ci/*/workflow.toml`.
    Validate {
        /// Specific `workflow.toml` files to check (default: scan
        /// `.alt/ci/<name>/workflow.toml`).
        #[arg(hint = path)]
        paths: Vec<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum WorkspaceOp {
    /// Create a workspace with its own working tree, checked out on a branch
    Add {
        /// Workspace name
        #[arg(hint = none)]
        name: String,
        /// Working-tree directory for the workspace
        #[arg(hint = dir)]
        path: std::path::PathBuf,
        /// Branch to check out (defaults to a new branch named after the
        /// workspace, at the current commit)
        #[arg(hint = "branch")]
        branch: Option<String>,
    },
    /// List the workspaces
    List,
    /// Remove a workspace (its HEAD ref and control dir; files are kept)
    Remove {
        /// Workspace name
        #[arg(hint = "workspace")]
        name: String,
    },
}

#[derive(Subcommand)]
pub enum IdentityOp {
    /// Generate a new Ed25519 keypair under `<alt-dir>/identity/<principal>.{pub,sec}`
    Init {
        /// Principal id (defaults to `$ALT_PRINCIPAL_ID` or `$USER`)
        #[arg(hint = none)]
        principal: Option<String>,
    },
    /// List installed identities (no secret material; only `.pub` rows)
    List,
    /// Add a public key to the trust store (`<alt-dir>/trust/<principal>.pub`)
    Trust {
        /// Principal id
        #[arg(hint = none)]
        principal: String,
        /// Path to a `.pub` file produced by `alt identity init`
        #[arg(hint = path)]
        pub_file: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
pub enum RemoteOp {
    /// Register a git remote — typically `https://host/user/repo.git`
    Add {
        /// Remote name (e.g. `origin`)
        #[arg(hint = none)]
        name: String,
        /// Remote URL
        #[arg(hint = none)]
        url: String,
    },
    /// List configured remotes
    List,
    /// Drop a configured remote (refs/remotes/<name> are not touched)
    Remove {
        /// Remote name
        #[arg(hint = "remote")]
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
    Start {
        #[arg(hint = none)]
        name: String,
    },
    /// Merge the topic back into its target(s) and delete it (atomic
    /// ref transaction + single op-log entry, like every other flow op)
    Finish {
        #[arg(hint = "flow-topic")]
        name: String,
    },
}
