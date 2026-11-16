use std::borrow::Cow;
use std::io::Write;

use alt_bytes::ByteSlice;
use alt_git_codec::ObjectId;
use alt_repo::Repository;

/// git's `logmsg_reencode` for the default UTF-8 output encoding: a commit
/// with an `encoding` header is converted to UTF-8 (whole buffer) and the
/// header line is dropped; same-encoding commits just lose the header; a
/// failed conversion leaves the buffer untouched, header included.
fn reencode<'a>(data: &'a [u8], converted: &mut Vec<u8>) -> Cow<'a, [u8]> {
    let Some(enc) = encoding_header(data) else {
        return Cow::Borrowed(data);
    };
    if enc.eq_ignore_ascii_case(b"utf-8") || enc.eq_ignore_ascii_case(b"utf8") {
        return Cow::Owned(strip_encoding_header(data));
    }
    // unknown charset or undecodable bytes: keep as-is, like git
    if crate::charset::decode_into(enc, data, converted) {
        Cow::Owned(strip_encoding_header(converted))
    } else {
        Cow::Borrowed(data)
    }
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

#[derive(alt_args::Args, Clone)]
pub struct LogArgs {
    /// pretty-print commits: raw | oneline (ignored with --json)
    #[arg(long, default_value = "oneline")]
    pretty: String,
    /// limit the number of commits
    #[arg(short = 'n')]
    max_count: Option<usize>,
    /// emit the commit list as a stable JSON object
    #[arg(long)]
    json: bool,
    /// show a per-commit patch (unified diff for text, compact chunk +
    /// perceptual summary for binary so large-file history doesn't blow up
    /// the terminal). Ignored with --json.
    #[arg(short = 'p', long = "patch")]
    patch: bool,
    /// with -p, show for a merge commit the combined diff of what the
    /// merge itself changed (hunks that differ from every parent)
    #[arg(long = "cc")]
    combined: bool,
    /// start revision
    #[arg(default_value = "HEAD")]
    rev: String,
}

impl LogArgs {
    /// Helper for `alt show <rev>`: walk one commit, raw header, always
    /// emit the patch. `json` follows the user's flag; the json shape is
    /// the same as `alt log --json -n 1` so downstream tools can rely on
    /// a single schema.
    pub fn for_show(rev: String, json: bool) -> Self {
        Self {
            pretty: "raw".to_string(),
            max_count: Some(1),
            json,
            patch: !json,
            combined: true,
            rev,
        }
    }
}

impl LogArgs {
    /// The same log, starting from `rev` rewritten by `map` (used to point a
    /// bare `HEAD` at the current workspace's HEAD).
    pub(crate) fn map_rev(mut self, map: impl Fn(&str) -> String) -> Self {
        self.rev = map(&self.rev);
        self
    }
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

    if args.json {
        return run_json(out, repo, start, limit);
    }

    let notes = crate::meta::Notes::of(repo)?;
    let mut first = true;
    let mut converted = Vec::new();
    // one for the whole log, made by the first patch: a log without
    // patches reads neither the config nor the size of the store
    let mut style = None;
    let mut walk = repo.rev_walk(start)?;
    for item in std::iter::from_fn(|| walk.next_with_data()).take(limit) {
        let (oid, _, data) = item?;
        let payload = reencode(&data, &mut converted);
        match args.pretty.as_str() {
            // raw shows the stored object as is
            "raw" => write_raw(out, &oid, &payload, &mut first)?,
            "oneline" => match crate::meta::Notes::message(&notes, repo, oid)? {
                Some(m) => write_oneline(out, &oid, format!("\n\n{m}").as_bytes())?,
                None => write_oneline(out, &oid, &payload)?,
            },
            other => {
                return Err(
                    format!("unsupported --pretty={other} (supported: raw, oneline)").into(),
                );
            }
        }
        if args.patch {
            // as in git, a blank line sets a patch apart from every header
            // but the one-line one, and a combined diff from any header
            use crate::patch::Section;
            let mut patch = Vec::new();
            let style = match &style {
                Some(style) => style,
                None => style.insert(crate::patch::style(repo)?),
            };
            let shown = crate::patch::write_commit(&mut patch, repo, &oid, args.combined, style)?;
            if shown == Section::Combined || (shown == Section::Patch && args.pretty != "oneline") {
                out.write_all(b"\n")?;
            }
            out.write_all(&patch)?;
        }
    }
    Ok(())
}

/// `log --json`: `{schema_version, commits:[{oid, tree, parents, author,
/// committer, message, note_message}]}`. `author`/`committer` are the raw
/// ident lines (`Name <email> ts tz`); `message` is the full commit message,
/// `note_message` the replacement a metadata note holds (or null).
fn run_json(
    out: &mut impl Write,
    repo: &Repository,
    start: ObjectId,
    limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::json::Json;
    let notes = crate::meta::Notes::of(repo)?;
    let mut commits = Vec::new();
    for item in repo.rev_walk(start)?.take(limit) {
        let (oid, commit) = item?;
        let opt = |v: Option<&alt_bytes::BStr>| match v {
            Some(s) => Json::str(s),
            None => Json::Null,
        };
        let parents: Vec<Json> = commit.parents().map(|p| Json::str(p.to_string())).collect();
        commits.push(Json::Object(vec![
            ("oid", Json::str(oid.to_string())),
            (
                "tree",
                match commit.tree() {
                    Some(t) => Json::str(t.to_string()),
                    None => Json::Null,
                },
            ),
            ("parents", Json::Array(parents)),
            ("author", opt(commit.author())),
            ("committer", opt(commit.committer())),
            ("message", Json::str(commit.message())),
            (
                "note_message",
                crate::meta::Notes::message(&notes, repo, oid)?.map_or(Json::Null, Json::str),
            ),
        ]));
    }
    let doc = Json::Object(vec![
        ("schema_version", Json::Num(1)),
        ("commits", Json::Array(commits)),
    ]);
    doc.write(out)?;
    out.write_all(b"\n")?;
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
