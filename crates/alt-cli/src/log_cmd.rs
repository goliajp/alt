use std::borrow::Cow;
use std::io::Write;

use alt_git_codec::ObjectId;
use alt_repo::Repository;
use bstr::ByteSlice;

/// git's `logmsg_reencode` for the default UTF-8 output encoding: a commit
/// with an `encoding` header is converted to UTF-8 (whole buffer) and the
/// header line is dropped; same-encoding commits just lose the header; a
/// failed conversion leaves the buffer untouched, header included.
fn reencode(data: &[u8]) -> Cow<'_, [u8]> {
    let Some(enc) = encoding_header(data) else {
        return Cow::Borrowed(data);
    };
    if enc.eq_ignore_ascii_case(b"utf-8") || enc.eq_ignore_ascii_case(b"utf8") {
        return Cow::Owned(strip_encoding_header(data));
    }
    let Some(encoding) = encoding_rs::Encoding::for_label(enc) else {
        return Cow::Borrowed(data); // unknown charset: keep as-is, like git
    };
    let (converted, _, had_errors) = encoding.decode(data);
    if had_errors {
        return Cow::Borrowed(data);
    }
    Cow::Owned(strip_encoding_header(converted.as_bytes()))
}

/// The value of a top-level `encoding` header, if any.
fn encoding_header(data: &[u8]) -> Option<&[u8]> {
    for line in data.lines() {
        if line.is_empty() {
            return None; // message reached
        }
        if let Some(value) = line.strip_prefix(b"encoding ") {
            return Some(value);
        }
    }
    None
}

fn strip_encoding_header(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut in_headers = true;
    for line in data.split_inclusive(|&b| b == b'\n') {
        if in_headers {
            if line == b"\n" {
                in_headers = false;
            } else if line.starts_with(b"encoding ") {
                continue;
            }
        }
        out.extend_from_slice(line);
    }
    out
}

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
        let obj = repo.read_object(&oid)?.expect("walked oid exists");
        let payload = reencode(&obj.data);
        match args.pretty.as_str() {
            "raw" => write_raw(out, &oid, &payload, &mut first)?,
            "oneline" => write_oneline(out, &oid, &payload)?,
            other => return Err(format!("unsupported --pretty={other} (M1: raw, oneline)").into()),
        }
    }
    Ok(())
}

/// `--pretty=raw`: `commit <oid>`, the stored header block verbatim, then
/// the message indented by four spaces. Entries separated by a blank line.
fn write_raw(
    out: &mut impl Write,
    oid: &ObjectId,
    payload: &[u8],
    first: &mut bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // headers verbatim from the (re-encoded) payload — no reserialization
    let split = payload
        .find(b"\n\n")
        .unwrap_or(payload.len().saturating_sub(1));
    let (headers, message) = (
        &payload[..split + 1],
        &payload[(split + 2).min(payload.len())..],
    );

    if !*first {
        out.write_all(b"\n")?;
    }
    *first = false;
    writeln!(out, "commit {oid}")?;
    out.write_all(headers)?;
    // every message line: 4-space indent + the line with trailing
    // whitespace stripped; interior blank lines come out as "    \n" (the
    // indent survives) but trailing blank lines are dropped entirely, and
    // an empty message gets no header/message separator line at all.
    // (All rules verified against real git output on the corpus.)
    let mut lines: Vec<&[u8]> = message.lines().map(|l| l.trim_end()).collect();
    while lines.last() == Some(&&b""[..]) {
        lines.pop();
    }
    if lines.is_empty() {
        return Ok(());
    }
    out.write_all(b"\n")?;
    for line in lines {
        out.write_all(b"    ")?;
        out.write_all(line)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// `--pretty=oneline`: `<oid> <subject>` — the subject folds the title
/// lines (up to the first blank line) into one space-separated line.
fn write_oneline(
    out: &mut impl Write,
    oid: &ObjectId,
    payload: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let message = match payload.find(b"\n\n") {
        Some(at) => &payload[at + 2..],
        None => &[][..],
    };
    // title lines fold into one space-separated line, trailing whitespace
    // stripped; the title ends at the first *blank* line in git's sense —
    // whitespace-only counts (verified against git on the corpus)
    let mut subject: Vec<u8> = Vec::new();
    for line in message.lines().take_while(|l| !l.trim_end().is_empty()) {
        if !subject.is_empty() {
            subject.push(b' ');
        }
        subject.extend_from_slice(line);
    }
    write!(out, "{oid} ")?;
    out.write_all(subject.trim_end())?;
    out.write_all(b"\n")?;
    Ok(())
}
