//! The `alt` CLI. M1 scope: read-only commands whose output is byte-exact
//! with git — `cat-file`, `rev-parse`, `log` (raw / oneline). M2 adds
//! `import` (.git → .alt migration).

use alt_cli::{log_cmd, native, quote};

use std::io::Write;
use std::process::ExitCode;

use alt_git_codec::{ObjectId, ObjectKind, RawObject, Tree};
use alt_repo::Repository;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "alt", version, disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Operate in the named parallel workspace instead of the default one
    #[arg(long, global = true)]
    workspace: Option<String>,
}

#[derive(Subcommand)]
enum Command {
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
enum WorkspaceOp {
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
enum FlowOp {
    /// Create the `develop` branch off `main` and switch to it
    Init,
    /// Feature-branch operations
    Feature {
        #[command(subcommand)]
        op: FlowTopicOp,
    },
}

#[derive(Subcommand)]
enum FlowTopicOp {
    /// Branch `feature/<name>` off develop and switch to it
    Start { name: String },
    /// Merge `feature/<name>` back into develop and delete it
    Finish { name: String },
}

#[derive(clap::Args)]
struct CatFileArgs {
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

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fatal: {e}");
            ExitCode::from(128)
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let stdout = std::io::stdout().lock();
    let mut out = std::io::BufWriter::new(stdout);
    let cwd = std::env::current_dir()?;
    let ws = cli.workspace.as_deref();

    // native .alt commands open (or create) their own repo, not a git one
    match &cli.command {
        Command::Init { dir } => {
            native::init(dir.clone(), &mut out)?;
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Add { paths, json } => {
            native::NativeRepo::discover_workspace(&cwd, ws)?.add(paths, *json, &mut out)?;
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Commit { message, json } => {
            native::NativeRepo::discover_workspace(&cwd, ws)?.commit(message, *json, &mut out)?;
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Status { json } => {
            native::NativeRepo::discover_workspace(&cwd, ws)?.status(*json, &mut out)?;
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Branch { name, delete, json } => {
            native::NativeRepo::discover_workspace(&cwd, ws)?.branch(
                name.clone(),
                delete.clone(),
                *json,
                &mut out,
            )?;
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Switch { name, create, json } => {
            native::NativeRepo::discover_workspace(&cwd, ws)?
                .switch(name, *create, *json, &mut out)?;
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Diff { cached, json } => {
            native::NativeRepo::discover_workspace(&cwd, ws)?.diff(*cached, *json, &mut out)?;
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Merge { branch, json } => {
            let conflicts =
                native::NativeRepo::discover_workspace(&cwd, ws)?.merge(branch, *json, &mut out)?;
            out.flush()?;
            // git exits 1 when a merge stops in conflict
            return Ok(if conflicts {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            });
        }
        Command::Flow { op, json } => {
            let mut repo = native::NativeRepo::discover_workspace(&cwd, ws)?;
            match op {
                FlowOp::Init => repo.flow_init(*json, &mut out)?,
                FlowOp::Feature {
                    op: FlowTopicOp::Start { name },
                } => repo.flow_feature_start(name, *json, &mut out)?,
                FlowOp::Feature {
                    op: FlowTopicOp::Finish { name },
                } => repo.flow_feature_finish(name, *json, &mut out)?,
            }
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Undo { json } => {
            native::NativeRepo::discover_workspace(&cwd, ws)?.undo(*json, &mut out)?;
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Workspace { op, json } => {
            let mut repo = native::NativeRepo::discover(&cwd)?;
            match op {
                WorkspaceOp::Add { name, path, branch } => {
                    repo.workspace_add(name, path, branch.as_deref(), *json, &mut out)?
                }
                WorkspaceOp::List => repo.workspace_list(*json, &mut out)?,
                WorkspaceOp::Remove { name } => repo.workspace_remove(name, *json, &mut out)?,
            }
            out.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    let repo = Repository::discover(&cwd)?;

    match cli.command {
        Command::RevParse { rev } => {
            let oid = resolve(&repo, &rev)?;
            writeln!(out, "{oid}")?;
        }
        Command::CatFile(args) => {
            let oid = resolve(&repo, &args.object)?;
            let obj = repo
                .read_object(&oid)?
                .ok_or_else(|| format!("object {oid} not found"))?;
            if args.show_type {
                writeln!(out, "{}", obj.kind)?;
            } else if args.show_size {
                writeln!(out, "{}", obj.data.len())?;
            } else if args.pretty {
                pretty_print(&mut out, &repo, &oid, &obj)?;
            } else {
                return Err("one of -t, -s or -p is required".into());
            }
        }
        Command::Log(args) => log_cmd::run(&mut out, &repo, args)?,
        Command::Export { target } => {
            if !repo.is_native() {
                return Err("export needs a .alt store; run inside one (see 'alt import')".into());
            }
            let report = alt_export::export_git(repo.git_dir(), &target)?;
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
            let report = alt_import::import_git(&repo, &alt_dir, &actor, timestamp_ms)?;
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
        Command::Init { .. }
        | Command::Add { .. }
        | Command::Commit { .. }
        | Command::Status { .. }
        | Command::Branch { .. }
        | Command::Switch { .. }
        | Command::Diff { .. }
        | Command::Merge { .. }
        | Command::Flow { .. }
        | Command::Undo { .. }
        | Command::Workspace { .. } => {
            unreachable!("native commands are dispatched before git discovery")
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
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
fn pretty_print(
    out: &mut impl Write,
    repo: &Repository,
    oid: &ObjectId,
    obj: &RawObject,
) -> Result<(), Box<dyn std::error::Error>> {
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
