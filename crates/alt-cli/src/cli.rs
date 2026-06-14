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
pub enum FlowOp {
    /// Create the `develop` branch off `main` and switch to it
    Init,
    /// Feature-branch operations
    Feature {
        #[command(subcommand)]
        op: FlowTopicOp,
    },
}

#[derive(Subcommand)]
pub enum FlowTopicOp {
    /// Branch `feature/<name>` off develop and switch to it
    Start { name: String },
    /// Merge `feature/<name>` back into develop and delete it
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
        Command::Diff { cached, json } => repo.diff(*cached, *json, out)?,
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
        },
        Command::Undo { json } => repo.undo(*json, out)?,
        Command::Workspace { op, json } => match op {
            WorkspaceOp::Add { name, path, branch } => {
                repo.workspace_add(name, path, branch.as_deref(), *json, out)?
            }
            WorkspaceOp::List => repo.workspace_list(*json, out)?,
            WorkspaceOp::Remove { name } => repo.workspace_remove(name, *json, out)?,
        },
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
/// commands open their own repository at `cwd`. The caller is responsible for
/// having refreshed `store` first.
pub fn run_on_store<W: Write>(
    cli: &Cli,
    store: &mut Store,
    cwd: &Path,
    id: Identity,
    out: &mut W,
) -> Res<u8> {
    match &cli.command {
        Command::Init { .. } => Err("the daemon does not serve 'init'".into()),
        c if is_native(c) => {
            let (_alt_dir, coord) = native::resolve_workspace(cwd, cli.workspace.as_deref())?;
            let mut repo = NativeRepo::attach(store, coord, id);
            run_native(&mut repo, c, out)
        }
        c => {
            let repo = Repository::discover(cwd)?;
            run_git(&repo, c, out)?;
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
