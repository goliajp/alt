//! The command surface, shared by the `alt` binary (direct) and the `altd`
//! daemon. The clap types and the per-command dispatch live here so both reuse
//! the exact same command logic — the daemon is a perf cache, not a fork of
//! behaviour. Native commands run against a [`NativeRepo`] bound to a [`Store`];
//! git-layer commands (`cat-file`/`rev-parse`/`log`/`import`/`export`) open
//! their own [`Repository`].

use std::io::Write;
use std::path::Path;

use alt_git_codec::{ObjectId, ObjectKind, RawObject, Tree};
use alt_repo::Repository;
use clap::{Parser, Subcommand};

use crate::native::{self, Identity, NativeRepo, Store};
use crate::{log_cmd, quote};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(name = "alt", version, disable_help_subcommand = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
    /// Operate in the named parallel workspace instead of the default one
    #[arg(long, global = true)]
    pub workspace: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Provide contents or details of repository objects
    CatFile(CatFileArgs),
    /// Pick out and massage parameters
    RevParse { rev: String },
    /// Show commit logs
    Log(log_cmd::LogArgs),
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
        /// Paths to stage; `.` stages the whole working tree
        paths: Vec<String>,
        /// Emit a structured JSON result instead of the human line
        #[arg(long)]
        json: bool,
    },
    /// Record staged changes as a new commit
    Commit {
        /// Commit message
        #[arg(short = 'm')]
        message: String,
        /// Emit the new commit/tree oids as a JSON object
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
    /// Show changes between the index and the working tree (or HEAD)
    Diff {
        /// Show staged changes (HEAD vs index) instead of unstaged
        #[arg(long, visible_alias = "staged")]
        cached: bool,
        /// Emit a structured JSON diff instead of unified text
        #[arg(long)]
        json: bool,
        /// Show an AST-level diff per supported language (A8b): item-level
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
    /// Manage parallel workspaces (isolated HEAD/index/working tree)
    Workspace {
        #[command(subcommand)]
        op: WorkspaceOp,
        /// Emit a structured JSON result instead of the human view
        #[arg(long, global = true)]
        json: bool,
    },
    /// Manage git remotes (M6/W3: persisted as `<alt-dir>/remotes/<name>`)
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
    /// Fetch refs + objects from a configured remote (M6/W4 — git smart-http v2)
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
    /// Push refs + objects to a configured remote (M6/W5 — git smart-http v1)
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
    /// Manage local identities + trusted public keys (M6/W7 — A5b)
    Identity {
        #[command(subcommand)]
        op: IdentityOp,
        /// Emit a structured JSON result instead of the human view
        #[arg(long, global = true)]
        json: bool,
    },
    /// Audit-view the op log: who did what, in order, with the parsed A5a
    /// principal and any ref changes carried in each op's payload.
    #[command(name = "op-log")]
    OpLog {
        /// Limit the number of entries shown (newest first; default = all)
        #[arg(short = 'n')]
        max_count: Option<usize>,
        /// Emit a structured JSON list instead of the human view
        #[arg(long)]
        json: bool,
        /// Check each op against the A5b signature sidecar + trust store
        /// (M6/W7); rows tagged `signed-ok` / `unsigned` / `bad-sig` /
        /// `untrusted`
        #[arg(long)]
        verify: bool,
    },
    /// Verify the `alt-sig` header on one or more commit objects (M10/W15).
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

#[derive(Subcommand)]
pub enum WorkspaceOp {
    /// Create a workspace with its own working tree, checked out on a branch
    Add {
        /// Workspace name
        name: String,
        /// Working-tree directory for the workspace
        path: std::path::PathBuf,
        /// Branch to check out (defaults to the current branch)
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
pub enum IdentityOp {
    /// Generate a new Ed25519 keypair under `<alt-dir>/identity/<principal>.{pub,sec}`
    Init {
        /// Principal id (defaults to `$ALT_PRINCIPAL_ID` or `$USER`)
        principal: Option<String>,
    },
    /// List installed identities (no secret material; only `.pub` rows)
    List,
    /// Add a public key to the trust store (`<alt-dir>/trust/<principal>.pub`)
    Trust {
        /// Principal id
        principal: String,
        /// Path to a `.pub` file produced by `alt identity init`
        pub_file: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
pub enum RemoteOp {
    /// Register a git remote — typically `https://host/user/repo.git`
    Add {
        /// Remote name (e.g. `origin`)
        name: String,
        /// Remote URL
        url: String,
    },
    /// List configured remotes
    List,
    /// Drop a configured remote (refs/remotes/<name> are not touched)
    Remove {
        /// Remote name
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

#[derive(clap::Args)]
pub struct CatFileArgs {
    /// show object type
    #[arg(short = 't', group = "op")]
    show_type: bool,
    /// show object size
    #[arg(short = 's', group = "op")]
    show_size: bool,
    /// pretty-print object's content
    #[arg(short = 'p', group = "op")]
    pretty: bool,
    object: String,
}

/// Whether a command runs against the native `.alt` store (vs the git layer or
/// repo creation). `init` is neither — it creates a fresh repo.
pub fn is_native(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Add { .. }
            | Command::Commit { .. }
            | Command::Status { .. }
            | Command::Branch { .. }
            | Command::Switch { .. }
            | Command::Diff { .. }
            | Command::Merge { .. }
            | Command::Flow { .. }
            | Command::Undo { .. }
            | Command::Workspace { .. }
            | Command::Remote { .. }
            | Command::Fetch { .. }
            | Command::Push { .. }
            | Command::Identity { .. }
            | Command::OpLog { .. }
            | Command::Verify { .. }
    )
}

/// Runs a native command against an attached repo, returning the process exit
/// code (1 when a merge stops in conflict, else 0).
pub fn run_native<W: Write>(repo: &mut NativeRepo, cmd: &Command, out: &mut W) -> Res<u8> {
    match cmd {
        Command::Add { paths, json } => repo.add(paths, *json, out)?,
        Command::Commit { message, json } => repo.commit(message, *json, out)?,
        Command::Status { json } => repo.status(*json, out)?,
        Command::Branch { name, delete, json } => {
            repo.branch(name.clone(), delete.clone(), *json, out)?
        }
        Command::Switch { name, create, json } => repo.switch(name, *create, *json, out)?,
        Command::Diff {
            cached,
            json,
            semantic,
        } => repo.diff(*cached, *json, *semantic, out)?,
        Command::Merge { branch, json } => {
            // git exits 1 when a merge stops in conflict
            return Ok(if repo.merge(branch, *json, out)? {
                1
            } else {
                0
            });
        }
        Command::Flow { op, json } => match op {
            FlowOp::Init => repo.flow_init(*json, out)?,
            FlowOp::Feature {
                op: FlowTopicOp::Start { name },
            } => repo.flow_feature_start(name, *json, out)?,
            FlowOp::Feature {
                op: FlowTopicOp::Finish { name },
            } => repo.flow_feature_finish(name, *json, out)?,
            FlowOp::Release {
                op: FlowTopicOp::Start { name },
            } => repo.flow_release_start(name, *json, out)?,
            FlowOp::Release {
                op: FlowTopicOp::Finish { name },
            } => repo.flow_release_finish(name, *json, out)?,
            FlowOp::Hotfix {
                op: FlowTopicOp::Start { name },
            } => repo.flow_hotfix_start(name, *json, out)?,
            FlowOp::Hotfix {
                op: FlowTopicOp::Finish { name },
            } => repo.flow_hotfix_finish(name, *json, out)?,
        },
        Command::Undo { json } => repo.undo(*json, out)?,
        Command::Workspace { op, json } => match op {
            WorkspaceOp::Add { name, path, branch } => {
                repo.workspace_add(name, path, branch.as_deref(), *json, out)?
            }
            WorkspaceOp::List => repo.workspace_list(*json, out)?,
            WorkspaceOp::Remove { name } => repo.workspace_remove(name, *json, out)?,
        },
        Command::Remote { op, json } => match op {
            RemoteOp::Add { name, url } => repo.remote_add(name, url, *json, out)?,
            RemoteOp::List => repo.remote_list(*json, out)?,
            RemoteOp::Remove { name } => repo.remote_remove(name, *json, out)?,
        },
        Command::Fetch {
            remote,
            refspecs,
            json,
        } => repo.fetch(remote, refspecs, *json, out)?,
        Command::Push {
            remote,
            refspecs,
            force,
            json,
        } => repo.push(remote, refspecs, *force, *json, out)?,
        Command::Identity { op, json } => match op {
            IdentityOp::Init { principal } => {
                repo.identity_init(principal.as_deref(), *json, out)?
            }
            IdentityOp::List => repo.identity_list(*json, out)?,
            IdentityOp::Trust {
                principal,
                pub_file,
            } => repo.identity_trust(principal, pub_file, *json, out)?,
        },
        Command::OpLog {
            max_count,
            json,
            verify,
        } => repo.op_log(*max_count, *json, *verify, out)?,
        Command::Verify {
            commits,
            max_count,
            json,
        } => repo.verify_commits(commits, *max_count, *json, out)?,
        _ => return Err("not a native command".into()),
    }
    Ok(0)
}

/// Runs a git-layer command against an opened repository.
pub fn run_git<W: Write>(repo: &Repository, cmd: &Command, out: &mut W) -> Res<()> {
    match cmd {
        Command::RevParse { rev } => {
            let oid = resolve(repo, rev)?;
            writeln!(out, "{oid}")?;
        }
        Command::CatFile(args) => {
            let oid = resolve(repo, &args.object)?;
            let obj = repo
                .read_object(&oid)?
                .ok_or_else(|| format!("object {oid} not found"))?;
            if args.show_type {
                writeln!(out, "{}", obj.kind)?;
            } else if args.show_size {
                writeln!(out, "{}", obj.data.len())?;
            } else if args.pretty {
                pretty_print(out, repo, &oid, &obj)?;
            } else {
                return Err("one of -t, -s or -p is required".into());
            }
        }
        Command::Log(args) => log_cmd::run(out, repo, args.clone())?,
        Command::Export { target } => {
            if !repo.is_native() {
                return Err("export needs a .alt store; run inside one (see 'alt import')".into());
            }
            let report = alt_export::export_git(repo.git_dir(), target)?;
            writeln!(
                out,
                "exported {} objects, {} refs{} into {}",
                report.objects,
                report.refs,
                if report.head { " + HEAD" } else { "" },
                target.join(".git").display()
            )?;
        }
        Command::Import { target } => {
            let alt_dir = target.join(".alt");
            let timestamp_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let actor = format!(
                "cli/import@{}",
                std::env::var("USER").as_deref().unwrap_or("unknown")
            );
            let report = alt_import::import_git(repo, &alt_dir, &actor, timestamp_ms)?;
            writeln!(
                out,
                "imported {} objects ({} new), {} refs ({} changed), \
                 {} lineage deltas into {}",
                report.objects_seen,
                report.objects_new,
                report.refs_seen,
                report.refs_changed,
                report.lineage_deltas,
                alt_dir.display()
            )?;
            match report.op {
                Some(op) => writeln!(out, "op {op}")?,
                None => writeln!(out, "already up to date, no op recorded")?,
            }
        }
        _ => return Err("not a git-layer command".into()),
    }
    Ok(())
}

/// Dispatches one command against a held store (the daemon path): native
/// commands resolve their workspace from `cwd` and attach to `store`; git-layer
/// commands run against the held `repo` (the daemon's own repository), so they
/// amortize the open just like native commands do — `log` reopens nothing per
/// request. The caller is responsible for having refreshed both `store` and
/// `repo` first.
pub fn run_on_store<W: Write>(
    cli: &Cli,
    store: &mut Store,
    repo: &Repository,
    cwd: &Path,
    id: Identity,
    request_id: Option<alt_refs::IdemKey>,
    out: &mut W,
) -> Res<u8> {
    match &cli.command {
        Command::Init { .. } => Err("the daemon does not serve 'init'".into()),
        c if is_native(c) => {
            let (_alt_dir, coord) = native::resolve_workspace(cwd, cli.workspace.as_deref())?;
            // the idempotency key is stamped on the command's terminal ref
            // transaction; the daemon's earlier `applied_request` check (in
            // dispatch) already short-circuited a completed duplicate
            let mut repo = NativeRepo::attach(store, coord, id, request_id);
            run_native(&mut repo, c, out)
        }
        c => {
            run_git(repo, c, out)?;
            Ok(0)
        }
    }
}

fn resolve(repo: &Repository, spec: &str) -> Result<ObjectId, String> {
    repo.rev_parse(spec)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "ambiguous argument '{spec}': unknown revision or path not in the working tree."
            )
        })
}

/// `cat-file -p`: blobs/commits/tags verbatim, trees in ls-tree-like shape.
fn pretty_print<W: Write>(
    out: &mut W,
    repo: &Repository,
    oid: &ObjectId,
    obj: &RawObject,
) -> Res<()> {
    match obj.kind {
        ObjectKind::Tree => {
            let quotepath = repo
                .config()
                .get_bool("core", None, "quotepath")
                .transpose()?
                .unwrap_or(true);
            let tree = Tree::parse(&obj.data, repo.algo())?;
            for entry in &tree.entries {
                write!(
                    out,
                    "{:06o} {} {}\t",
                    entry.mode.value(),
                    entry.mode.object_kind(),
                    entry.oid
                )?;
                quote::write_path(out, &entry.name, quotepath)?;
                out.write_all(b"\n")?;
            }
        }
        _ => out.write_all(&obj.data)?,
    }
    let _ = oid;
    Ok(())
}
