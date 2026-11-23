use std::io::Write;

use alt_git_codec::ObjectId;
use alt_repo::Repository;

use crate::log_format::{reencode, write_oneline, write_raw};
use crate::log_range::Selection;
use crate::native::pathspec::Place;
use crate::rev_or_path::{Tells, Words};

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
    /// A, `A...B` for what leads to one of them and not to both; then
    /// paths, to show only the commits that change them
    #[arg(id = "rev", value_name = "REV", default_value = "HEAD", hint = "rev")]
    words: Vec<String>,
    /// paths, where a word before them could be taken for a revision
    #[arg(last = true, hint = "path-tracked")]
    paths: Vec<String>,
    #[arg(dashed = true)]
    dashed: bool,
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
            words: vec![rev],
            paths: Vec::new(),
            dashed: true,
        }
    }

    /// The same log, of the one revision and the paths its words come to
    /// where the command runs `at`.
    pub(crate) fn told_apart(mut self, repo: &Repository, at: &Place) -> Res<Self> {
        let resolves = |rev: &str| Ok(repo.rev_parse(&at.at_head(rev))?.is_some());
        let tells = Tells {
            revision: |word: &str| match crate::log_range::sides(word) {
                Some((left, right, _)) => Ok(resolves(left)? && resolves(right)?),
                None => resolves(word),
            },
            file: |word: &str| Ok(at.root.join(at.path(word)?).symlink_metadata().is_ok()),
        };
        // `HEAD` alone is what no word at all comes to as well
        let given = self.dashed || self.words != ["HEAD"];
        let words = Words {
            before: &self.words,
            dashed: self.dashed,
            after: &self.paths,
        };
        let (revs, paths) = match given {
            true => words.revisions_then_paths(&tells)?,
            false => (&self.words[..], &self.paths[..]),
        };
        let rev = match revs {
            [] => "HEAD".to_owned(),
            [rev] => rev.clone(),
            _ => {
                let usage = "log takes one revision or one range";
                return Err(crate::machine::Failure::usage(usage).into());
            }
        };
        (self.paths, self.words) = (paths.to_vec(), vec![rev]);
        Ok(self)
    }

    fn rev(&self) -> &str {
        &self.words[0]
    }
}

impl LogArgs {
    /// The same log, with each revision it names rewritten by `map` (used
    /// to point a bare `HEAD` at the current workspace's HEAD).
    pub(crate) fn map_rev(mut self, map: impl Fn(&str) -> String) -> Self {
        self.words[0] = match crate::log_range::sides(self.rev()) {
            Some((left, right, both)) => {
                let dots = if both { "..." } else { ".." };
                format!("{}{dots}{}", map(left), map(right))
            }
            None => map(self.rev()),
        };
        self
    }

    /// One commit on one line: its id, abbreviated with `--oneline`, and
    /// its subject, that of `note` where a note has one for it.
    fn write_line(
        &self,
        out: &mut impl Write,
        repo: &Repository,
        oid: &ObjectId,
        note: Option<String>,
        payload: &[u8],
    ) -> Res<()> {
        let shown = match (self.oneline, repo.abbrev()?) {
            (false, _) => oid.to_string(),
            (true, Some(abbrev)) => abbrev.of(oid)?,
            // a plain git repository is not asked how many it holds
            (true, None) => oid.to_string()[..7].to_owned(),
        };
        match note {
            Some(message) => write_oneline(out, &shown, format!("\n\n{message}").as_bytes()),
            None => write_oneline(out, &shown, payload),
        }
    }

    /// The revision as it was given.
    pub(crate) fn named(&self) -> &str {
        self.rev()
    }

    /// The commits this log is of, newest first, each with the bytes of
    /// its object. `named` is the revision as it was given.
    fn commits<'r>(&self, repo: &'r Repository, at: &Place, named: &str) -> Res<Box<Walked<'r>>> {
        let limit = self.max_count.unwrap_or(usize::MAX);
        if self.paths.is_empty() && crate::log_range::sides(self.rev()).is_none() {
            let start = repo
                .rev_parse(self.rev())?
                .ok_or_else(|| format!("bad revision '{}'", self.rev()))?;
            let mut walk = repo.rev_walk(start)?;
            let walked = std::iter::from_fn(move || walk.next_with_data());
            let commits = walked.map(|item| item.map(|(oid, _, data)| (oid, data)));
            return Ok(Box::new(commits.map(|item| Ok(item?)).take(limit)));
        }
        let paths: Vec<String> = self.paths.iter().map(|p| at.path(p)).collect::<Res<_>>()?;
        let selected = Selection::of(repo, self.rev(), named)?.commits(repo, &paths, limit)?;
        Ok(Box::new(selected.into_iter().map(Ok)))
    }
}

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The commits of a log, each with the bytes of its object.
type Walked<'r> = dyn Iterator<Item = Res<(ObjectId, Vec<u8>)>> + 'r;

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
                let note = crate::meta::Notes::message(&notes, repo, oid)?;
                args.write_line(out, repo, &oid, note, &payload)?
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
