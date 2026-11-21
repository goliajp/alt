//! Running a git-layer command, and either kind against a held store.

use std::io::Write;
use std::path::Path;

use alt_git_codec::{ObjectId, ObjectKind, RawObject, Tree};
use alt_repo::Repository;

use super::*;
use crate::native::pathspec::Place;
use crate::native::{self, Identity, NativeRepo, Store};
use crate::{log_cmd, quote};

#[derive(alt_args::Args)]
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
    #[arg(hint = "rev")]
    object: String,
}

/// Runs a git-layer command against an opened repository, for the
/// workspace and the directory of this invocation.
pub fn run_git<W: Write>(repo: &Repository, cmd: &Command, at: &Place, out: &mut W) -> Res<()> {
    let at_head = |spec: &str| at.at_head(spec);
    match cmd {
        Command::RevParse { rev } => {
            let oid = resolve(repo, &at_head(rev))?;
            writeln!(out, "{oid}")?;
        }
        Command::CatFile(args) => {
            let oid = resolve(repo, &at_head(&args.object))?;
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
        Command::Log(args) => log_cmd::run(out, repo, args.clone().map_rev(at_head), &at.head)?,
        Command::Show { rev, json } => {
            let spec = at_head(rev.as_deref().unwrap_or("HEAD"));
            // `<rev>:<path>` selects a blob at `path` as of commit
            // `rev`, matching git show's revision-blob form. The
            // commit + diff path stays for everything else.
            if let Some((rev_part, path_part)) = spec.split_once(':') {
                if *json {
                    return Err("--json is not supported with <rev>:<path>".into());
                }
                let blob = at.blob(repo, rev_part, path_part)?;
                out.write_all(&blob)?;
                return Ok(());
            }
            let args = crate::log_cmd::LogArgs::for_show(spec, *json);
            crate::log_cmd::run(out, repo, args, &at.head)?;
        }
        Command::Blame {
            path,
            rev,
            moves,
            copies,
            follow: _,
        } => {
            let options = crate::blame::Options::of(moves, copies);
            at.blame(out, repo, path, rev.as_deref(), options)?;
        }
        Command::Config {
            key,
            value,
            list,
            unset,
        } => {
            crate::config_cmd::run(
                repo,
                key.as_deref(),
                value.as_deref(),
                *list,
                unset.as_deref(),
                out,
            )?;
        }
        Command::Export { target } => {
            if !repo.is_native() {
                return Err("export needs a .alt store; run inside one (see 'alt import')".into());
            }
            let report = crate::native::export_git(repo.git_dir(), target)?;
            writeln!(
                out,
                "exported {} objects, {} refs{} into {}",
                report.objects,
                report.refs,
                if report.head { " + HEAD" } else { "" },
                target.join(".git").display()
            )?;
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
    id: &Identity,
    request_id: Option<alt_refs::IdemKey>,
    out: &mut W,
) -> Res<u8> {
    match &cli.command {
        Command::Init { .. } => Err("the daemon does not serve 'init'".into()),
        Command::Fsck { json } => crate::fsck::run(store, *json, out),
        c if is_native(c) => {
            let (_alt_dir, coord) = native::resolve_workspace(cwd, cli.workspace.as_deref())?;
            // Execution has already claimed the whole request. The id here
            // attributes each operation; it is not a ref transaction dedup key.
            let mut repo = NativeRepo::attach(store, coord, id.clone(), request_id);
            run_native(&mut repo, c, out)
        }
        c => {
            let at = Place::of(repo, cwd, cli.workspace.as_deref())?;
            run_git(repo, c, &at, out)?;
            Ok(0)
        }
    }
}

fn resolve(repo: &Repository, spec: &str) -> Res<ObjectId> {
    repo.rev_parse(spec)?.ok_or_else(|| {
        format!("ambiguous argument '{spec}': unknown revision or path not in the working tree.")
            .into()
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
            let quotepath = crate::git_settings::repository(repo)?
                .get_bool("core", None, "quotepath")
                .transpose()?
                .unwrap_or(true);
            let tree = Tree::parse(&obj.data, repo.algo()?)?;
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
