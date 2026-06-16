use std::borrow::Cow;
use std::io::Write;

use alt_git_codec::{Commit, ObjectId, ObjectKind, Tree};
use alt_repo::Repository;
use bstr::{BString, ByteSlice};

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

#[derive(clap::Args, Clone)]
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

    if args.json {
        return run_json(out, repo, start, limit);
    }

    let mut first = true;
    // Flattened-tree cache shared across the walk. rev_walk visits N, N-1,
    // N-2, … — so each commit's parent tree (just flattened as "old") shows
    // up next iteration as the current commit's "new". Caching by commit
    // oid drops the flatten count from ≈2·N to N+1 on a linear history.
    let mut tree_cache: std::collections::HashMap<ObjectId, std::rc::Rc<Vec<TreeFile>>> =
        std::collections::HashMap::new();
    for item in repo.rev_walk(start)?.take(limit) {
        let (oid, _) = item?;
        let obj = repo.read_object(&oid)?.expect("walked oid exists");
        let payload = reencode(&obj.data);
        match args.pretty.as_str() {
            "raw" => write_raw(out, &oid, &payload, &mut first)?,
            "oneline" => write_oneline(out, &oid, &payload)?,
            other => return Err(format!("unsupported --pretty={other} (M1: raw, oneline)").into()),
        }
        if args.patch {
            emit_patch_for_commit(out, repo, &oid, &mut tree_cache)?;
        }
    }
    Ok(())
}

/// One file in a flattened tree, identified by its full path. Built only
/// for the patch path — keep it self-contained so log_cmd doesn't depend on
/// alt-worktree's working-tree types.
struct TreeFile {
    path: BString,
    oid: ObjectId,
    mode: u32,
}

/// Recursive walk of a tree object into path-sorted file entries. Gitlinks
/// are kept (so the patch surfaces a submodule oid change) but are treated
/// as opaque — never read as blob content. Subtrees recurse; anything else
/// is a leaf.
fn flatten_tree(
    repo: &Repository,
    tree_oid: ObjectId,
    prefix: &mut Vec<u8>,
    out: &mut Vec<TreeFile>,
) -> Result<(), Box<dyn std::error::Error>> {
    let obj = repo
        .read_object(&tree_oid)?
        .ok_or_else(|| format!("tree {tree_oid} missing"))?;
    if obj.kind != ObjectKind::Tree {
        return Err(format!("{tree_oid} is not a tree").into());
    }
    let tree = Tree::parse(&obj.data, repo.algo())?;
    for e in tree.entries {
        let mark = prefix.len();
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        prefix.extend_from_slice(e.name.as_bytes());
        if e.mode.object_kind() == ObjectKind::Tree {
            flatten_tree(repo, e.oid, prefix, out)?;
        } else {
            out.push(TreeFile {
                path: BString::from(prefix.clone()),
                oid: e.oid,
                mode: e.mode.value(),
            });
        }
        prefix.truncate(mark);
    }
    Ok(())
}

/// Flattens a commit's full file set. The first parent (or an empty tree
/// for a root commit) is what we diff against; merges show the first-parent
/// patch only — matches git's `log -p` default. Cached: a commit oid is
/// flattened at most once per `log -p` run (the caller passes a shared
/// `HashMap`).
fn entries_for(
    repo: &Repository,
    commit: &ObjectId,
    cache: &mut std::collections::HashMap<ObjectId, std::rc::Rc<Vec<TreeFile>>>,
) -> Result<std::rc::Rc<Vec<TreeFile>>, Box<dyn std::error::Error>> {
    if let Some(hit) = cache.get(commit) {
        return Ok(hit.clone());
    }
    let obj = repo
        .read_object(commit)?
        .ok_or_else(|| format!("commit {commit} missing"))?;
    let parsed = Commit::parse(&obj.data)?;
    let tree = parsed
        .tree()
        .ok_or_else(|| format!("commit {commit} has no tree header"))?;
    let mut out = Vec::new();
    flatten_tree(repo, tree, &mut Vec::new(), &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    let rc = std::rc::Rc::new(out);
    cache.insert(*commit, rc.clone());
    Ok(rc)
}

/// Walks one commit + its first parent and writes the differences as a
/// stream of `diff --git` stanzas. Binary blobs land as a compact
/// chunk + perceptual summary so a single 100 MiB image doesn't paste a
/// megabyte of header noise (or worse, the bytes) into the terminal.
fn emit_patch_for_commit(
    out: &mut impl Write,
    repo: &Repository,
    commit: &ObjectId,
    cache: &mut std::collections::HashMap<ObjectId, std::rc::Rc<Vec<TreeFile>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let obj = repo
        .read_object(commit)?
        .ok_or_else(|| format!("commit {commit} missing"))?;
    let parsed = Commit::parse(&obj.data)?;
    let new_files = entries_for(repo, commit, cache)?;
    let empty_parent: std::rc::Rc<Vec<TreeFile>> = std::rc::Rc::new(Vec::new());
    let old_files = match parsed.parents().next() {
        Some(p) => entries_for(repo, &p, cache)?,
        None => empty_parent,
    };

    let (mut i, mut j) = (0, 0);
    while i < old_files.len() || j < new_files.len() {
        let cmp = match (old_files.get(i), new_files.get(j)) {
            (Some(o), Some(n)) => o.path.cmp(&n.path),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => break,
        };
        match cmp {
            std::cmp::Ordering::Less => {
                let o = &old_files[i];
                emit_file_stanza(out, repo, &o.path, Some(o), None)?;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                let n = &new_files[j];
                emit_file_stanza(out, repo, &n.path, None, Some(n))?;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                let o = &old_files[i];
                let n = &new_files[j];
                if o.oid != n.oid || o.mode != n.mode {
                    emit_file_stanza(out, repo, &n.path, Some(o), Some(n))?;
                }
                i += 1;
                j += 1;
            }
        }
    }
    Ok(())
}

fn read_blob_bytes(
    repo: &Repository,
    oid: &ObjectId,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let obj = repo
        .read_object(oid)?
        .ok_or_else(|| format!("blob {oid} missing"))?;
    Ok(obj.data)
}

fn emit_file_stanza(
    out: &mut impl Write,
    repo: &Repository,
    path: &BString,
    old: Option<&TreeFile>,
    new: Option<&TreeFile>,
) -> Result<(), Box<dyn std::error::Error>> {
    let path_str = path.to_str_lossy();
    writeln!(out, "diff --git a/{path_str} b/{path_str}")?;
    match (old, new) {
        (None, Some(n)) => writeln!(out, "new file mode {:06o}", n.mode)?,
        (Some(o), None) => writeln!(out, "deleted file mode {:06o}", o.mode)?,
        (Some(o), Some(n)) if o.mode != n.mode => {
            writeln!(out, "old mode {:06o}", o.mode)?;
            writeln!(out, "new mode {:06o}", n.mode)?;
        }
        _ => {}
    }
    let abbrev = |oid: Option<&ObjectId>| match oid {
        Some(o) => o.to_string().get(..7).unwrap_or("").to_owned(),
        None => "0000000".into(),
    };
    writeln!(
        out,
        "index {}..{}",
        abbrev(old.map(|o| &o.oid)),
        abbrev(new.map(|n| &n.oid)),
    )?;

    // Gitlinks: never read as a blob (the oid is a submodule commit that
    // lives in another repo); show the oid change line and move on.
    if old.is_some_and(|o| o.mode == 0o160000) || new.is_some_and(|n| n.mode == 0o160000) {
        writeln!(out, "Subproject commit change")?;
        return Ok(());
    }

    let old_bytes = match old {
        Some(o) => read_blob_bytes(repo, &o.oid)?,
        None => Vec::new(),
    };
    let new_bytes = match new {
        Some(n) => read_blob_bytes(repo, &n.oid)?,
        None => Vec::new(),
    };

    if alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes) {
        // Compact summary: chunk-level dedup ratio + a perceptual hint
        // when the content is a recognised image kind. Never dump raw
        // bytes — `alt log -p` on a binary-asset history would otherwise
        // be unusable.
        writeln!(out, "Binary files a/{path_str} and b/{path_str} differ")?;
        let cd =
            alt_diff::binary::chunk_diff(&old_bytes, &new_bytes, alt_diff::binary::DEFAULT_PARAMS);
        let pct = (cd.byte_shared_ratio() * 100.0).round() as u32;
        writeln!(
            out,
            "chunks: {} shared, {} added, {} removed ({pct}% bytes shared)",
            cd.shared_chunks, cd.added_chunks, cd.removed_chunks,
        )?;
        let old_fp = alt_diff::perceptual::fingerprint(&old_bytes);
        let new_fp = alt_diff::perceptual::fingerprint(&new_bytes);
        if let Some(d) = alt_diff::perceptual::distance(old_fp, new_fp) {
            let kind = old_fp.unwrap().kind.as_str();
            let pct_off = (d * 100.0).round() as u32;
            writeln!(out, "perceptual diff: {pct_off}% off (prism={kind})")?;
        }
        return Ok(());
    }

    match old {
        Some(_) => writeln!(out, "--- a/{path_str}")?,
        None => writeln!(out, "--- /dev/null")?,
    }
    match new {
        Some(_) => writeln!(out, "+++ b/{path_str}")?,
        None => writeln!(out, "+++ /dev/null")?,
    }
    let mut buf = Vec::new();
    alt_diff::write_unified(&mut buf, &old_bytes, &new_bytes, 3);
    out.write_all(&buf)?;
    Ok(())
}

/// `log --json`: `{schema_version, commits:[{oid, tree, parents, author,
/// committer, message}]}`. `author`/`committer` are the raw ident lines
/// (`Name <email> ts tz`); `message` is the full commit message.
fn run_json(
    out: &mut impl Write,
    repo: &Repository,
    start: ObjectId,
    limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::json::Json;
    let mut commits = Vec::new();
    for item in repo.rev_walk(start)?.take(limit) {
        let (oid, _) = item?;
        let obj = repo.read_object(&oid)?.expect("walked oid exists");
        let commit = alt_git_codec::Commit::parse(&obj.data)?;
        let opt = |v: Option<&bstr::BStr>| match v {
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
