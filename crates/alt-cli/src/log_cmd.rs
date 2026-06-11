use std::io::Write;

use alt_git_codec::ObjectId;
use alt_repo::Repository;
use bstr::ByteSlice;

#[derive(clap::Args)]
pub struct LogArgs {
    /// pretty-print commits: raw | oneline
    #[arg(long)]
    pretty: String,
    /// limit the number of commits
    #[arg(short = 'n')]
    max_count: Option<usize>,
    /// start revision
    #[arg(default_value = "HEAD")]
    rev: String,
}

pub fn run(
    out: &mut impl Write,
    repo: &Repository,
    args: LogArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = repo
        .rev_parse(&args.rev)?
        .ok_or_else(|| format!("bad revision '{}'", args.rev))?;
    let limit = args.max_count.unwrap_or(usize::MAX);

    let mut first = true;
    for item in repo.rev_walk(start)?.take(limit) {
        let (oid, _) = item?;
        match args.pretty.as_str() {
            "raw" => write_raw(out, repo, &oid, &mut first)?,
            "oneline" => write_oneline(out, repo, &oid)?,
            other => return Err(format!("unsupported --pretty={other} (M1: raw, oneline)").into()),
        }
    }
    Ok(())
}

/// `--pretty=raw`: `commit <oid>`, the stored header block verbatim, then
/// the message indented by four spaces. Entries separated by a blank line.
fn write_raw(
    out: &mut impl Write,
    repo: &Repository,
    oid: &ObjectId,
    first: &mut bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let obj = repo.read_object(oid)?.expect("walked oid exists");
    // headers verbatim from the object payload — no reserialization
    let split = obj
        .data
        .find(b"\n\n")
        .unwrap_or(obj.data.len().saturating_sub(1));
    let (headers, message) = (
        &obj.data[..split + 1],
        &obj.data[(split + 2).min(obj.data.len())..],
    );

    if !*first {
        out.write_all(b"\n")?;
    }
    *first = false;
    writeln!(out, "commit {oid}")?;
    out.write_all(headers)?;
    out.write_all(b"\n")?;
    for line in message.lines() {
        if line.is_empty() {
            out.write_all(b"\n")?;
        } else {
            out.write_all(b"    ")?;
            out.write_all(line)?;
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

/// `--pretty=oneline`: `<oid> <subject>` — the subject folds the title
/// lines (up to the first blank line) into one space-separated line.
fn write_oneline(
    out: &mut impl Write,
    repo: &Repository,
    oid: &ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    let obj = repo.read_object(oid)?.expect("walked oid exists");
    let message = match obj.data.find(b"\n\n") {
        Some(at) => &obj.data[at + 2..],
        None => &[][..],
    };
    write!(out, "{oid} ")?;
    let mut first = true;
    for line in message.lines().take_while(|l| !l.is_empty()) {
        if !first {
            out.write_all(b" ")?;
        }
        first = false;
        out.write_all(line)?;
    }
    out.write_all(b"\n")?;
    Ok(())
}
