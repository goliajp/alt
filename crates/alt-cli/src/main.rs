//! The `alt` CLI. M1 scope: read-only commands whose output is byte-exact
//! with git — `cat-file`, `rev-parse`, `log` (raw / oneline). M2 adds
//! `import` (.git → .alt migration).

mod log_cmd;
mod quote;

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
    let repo = Repository::discover(&std::env::current_dir()?)?;
    let stdout = std::io::stdout().lock();
    let mut out = std::io::BufWriter::new(stdout);

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
