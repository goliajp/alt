use std::io::Write;

use alt_bytes::ByteSlice;
use alt_git_codec::ObjectId;
use alt_repo::Repository;

use crate::log_encoding::reencode;
use crate::log_range::Selection;
use crate::native::pathspec::Place;

#[derive(alt_args::Args, Clone)]
pub struct LogArgs {
    /// pretty-print commits: raw | oneline (ignored with --json)
    #[arg(long, default_value = "oneline", hint = none)]
    pretty: String,
    /// limit the number of commits
    #[arg(short = 'n', hint = none)]
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
    /// each commit as its abbreviated id and its subject
    #[arg(long)]
    oneline: bool,
    /// start revision, or a range: `A..B` for what leads to B and not to
    /// A, `A...B` for what leads to one of them and not to both
    #[arg(default_value = "HEAD", hint = "rev")]
    rev: String,
    /// show only the commits that change these paths
    #[arg(last = true, hint = "path-tracked")]
    paths: Vec<String>,
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
            oneline: false,
            rev,
            paths: Vec::new(),
        }
    }
}

impl LogArgs {
    /// The same log, with each revision it names rewritten by `map` (used
    /// to point a bare `HEAD` at the current workspace's HEAD).
    pub(crate) fn map_rev(mut self, map: impl Fn(&str) -> String) -> Self {
        self.rev = match crate::log_range::sides(&self.rev) {
            Some((left, right, both)) => {
                let dots = if both { "..." } else { ".." };
                format!("{}{dots}{}", map(left), map(right))
            }
            None => map(&self.rev),
        };
        self
    }

    /// The revision as it was given.
    pub(crate) fn named(&self) -> &str {
        &self.rev
    }

    /// The commits this log is of, newest first, each with the bytes of
    /// its object. `named` is the revision as it was given.
    fn commits<'r>(
        &self,
        repo: &'r Repository,
        at: &Place,
        named: &str,
    ) -> Res<Box<dyn Iterator<Item = Res<(ObjectId, Vec<u8>)>> + 'r>> {
        let limit = self.max_count.unwrap_or(usize::MAX);
        if self.paths.is_empty() && crate::log_range::sides(&self.rev).is_none() {
            let start = repo
                .rev_parse(&self.rev)?
                .ok_or_else(|| format!("bad revision '{}'", self.rev))?;
            let mut walk = repo.rev_walk(start)?;
            let walked = std::iter::from_fn(move || walk.next_with_data());
            let commits = walked.map(|item| item.map(|(oid, _, data)| (oid, data)));
            return Ok(Box::new(commits.map(|item| Ok(item?)).take(limit)));
        }
        let paths: Vec<String> = self.paths.iter().map(|p| at.path(p)).collect::<Res<_>>()?;
        let selected = Selection::of(repo, &self.rev, named)?.commits(repo, &paths, limit)?;
        Ok(Box::new(selected.into_iter().map(Ok)))
    }
}

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// `at` is where the command runs: its HEAD stands for `HEAD`, and its
/// directory is what paths are named from. `named` is the revision as it
/// was given.
pub fn run(
    out: &mut impl Write,
    repo: &Repository,
    args: LogArgs,
    at: &Place,
    named: &str,
) -> Res<()> {
    let commits = args.commits(repo, at, named)?;
    if args.json {
        return run_json(out, repo, commits);
    }
    let notes = crate::meta::Notes::of(repo)?;
    let mut first = true;
    let mut converted = Vec::new();
    // one for the whole log, made by the first patch: a log without
    // patches reads neither the config nor the size of the store
    let mut style = None;
    let pretty = if args.oneline {
        "oneline"
    } else {
        args.pretty.as_str()
    };
    for item in commits {
        let (oid, data) = item?;
        let payload = reencode(&data, &mut converted);
        match pretty {
            // raw shows the stored object as is
            "raw" => write_raw(out, &oid, &payload, &mut first)?,
            "oneline" => {
                let shown = match args.oneline {
                    false => oid.to_string(),
                    // a plain git repository is not asked how many it holds
                    true => match repo.abbrev()? {
                        Some(abbrev) => abbrev.of(&oid)?,
                        None => oid.to_string()[..7].to_owned(),
                    },
                };
                match crate::meta::Notes::message(&notes, repo, oid)? {
                    Some(m) => write_oneline(out, &shown, format!("\n\n{m}").as_bytes())?,
                    None => write_oneline(out, &shown, &payload)?,
                }
            }
            other => {
                return Err(
                    format!("unsupported --pretty={other} (supported: raw, oneline)").into(),
                );
            }
        }
        if args.patch {
            write_patch(out, repo, &args, &oid, &at.head, &mut style)?;
        }
    }
    Ok(())
}

/// One commit's patch, after its header; makes `style` if no patch has yet.
fn write_patch<'r>(
    out: &mut impl Write,
    repo: &'r Repository,
    args: &LogArgs,
    oid: &ObjectId,
    head: &str,
    style: &mut Option<crate::file_header::Style<'r>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // as in git, a blank line sets a patch apart from every header
    // but the one-line one, and a combined diff from any header
    use crate::patch::Section;
    let mut patch = Vec::new();
    let style = match &*style {
        Some(style) => style,
        None => style.insert(crate::patch::style(repo, head)?),
    };
    let shown = crate::patch::write_commit(&mut patch, repo, oid, args.combined, style)?;
    let one_line = args.oneline || args.pretty == "oneline";
    if shown == Section::Combined || (shown == Section::Patch && !one_line) {
        out.write_all(b"\n")?;
    }
    out.write_all(&patch)?;
    Ok(())
}

/// `log --json`: `{schema_version, commits:[{oid, tree, parents, author,
/// committer, message, note_message}]}`. `author`/`committer` are the raw
/// ident lines (`Name <email> ts tz`); `message` is the full commit message,
/// `note_message` the replacement a metadata note holds (or null).
fn run_json(
    out: &mut impl Write,
    repo: &Repository,
    walked: impl Iterator<Item = Res<(ObjectId, Vec<u8>)>>,
) -> Res<()> {
    use crate::json::Json;
    let notes = crate::meta::Notes::of(repo)?;
    let mut commits = Vec::new();
    for item in walked {
        let (oid, data) = item?;
        let commit = alt_git_codec::Commit::parse(&data)?;
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

/// `--pretty=oneline`: `<oid> <subject>`, the id as `oid` has it — the subject folds the title
/// lines (up to the first blank line) into one space-separated line.
fn write_oneline(out: &mut impl Write, oid: &str, payload: &[u8]) -> Res<()> {
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
