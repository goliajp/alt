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
            emit_patch_for_commit(out, repo, &oid)?;
        }
    }
    Ok(())
}

/// One file in a tree as seen by the patch path: its oid and mode. The
/// path is threaded separately (as a borrowed slice into the prefix
/// buffer) because `tree_diff` already has it on hand and stashing a
/// fresh `BString` per leaf would dominate the cost of a small patch.
struct TreeFile {
    oid: ObjectId,
    mode: u32,
}

// Pre-A3b history: `flatten_tree` + `entries_for` walked both trees and
// merged. tree_diff (below) supersedes them with a lockstep equal-oid
// prune that touches O(changed leaves × depth) trees instead of the
// whole tree. The flatten implementation was removed with the A3b
// refactor; if a future caller needs a full flattened tree view, lift
// the helper from `alt-worktree::flatten_tree` (which takes a NativeOdb).

/// Recursively diffs two trees, emitting only the changed leaves. Equal
/// (same-oid) subtrees are skipped wholesale — that's the whole point: a
/// commit that touched 1-3 files in a 50k-file monorepo shares every
/// subtree but a single deep path with its parent, and we descend only
/// into the differing branches. O(changes × depth) tree reads instead of
/// O(whole-tree) flatten-both-then-merge.
type TreeEmit<'a> = dyn FnMut(&[u8], Option<TreeFile>, Option<TreeFile>) -> Result<(), Box<dyn std::error::Error>>
    + 'a;

fn tree_diff(
    repo: &Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    prefix: &mut Vec<u8>,
    emit: &mut TreeEmit<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    if old == new {
        return Ok(()); // identical subtree (or both absent) — nothing to emit
    }
    let read_tree =
        |oid: ObjectId| -> Result<Vec<alt_git_codec::TreeEntry>, Box<dyn std::error::Error>> {
            let obj = repo
                .read_object(&oid)?
                .ok_or_else(|| format!("tree {oid} missing"))?;
            if obj.kind != ObjectKind::Tree {
                return Err(format!("{oid} is not a tree").into());
            }
            Ok(Tree::parse(&obj.data, repo.algo())?.entries)
        };

    let old_entries = match old {
        Some(o) => read_tree(o)?,
        None => Vec::new(),
    };
    let new_entries = match new {
        Some(n) => read_tree(n)?,
        None => Vec::new(),
    };

    let (mut i, mut j) = (0, 0);
    while i < old_entries.len() || j < new_entries.len() {
        let cmp = match (old_entries.get(i), new_entries.get(j)) {
            (Some(o), Some(n)) => o.name.cmp(&n.name),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => break,
        };
        type PushBody<'a> = dyn FnMut(&mut Vec<u8>) -> Result<(), Box<dyn std::error::Error>> + 'a;
        let push = |prefix: &mut Vec<u8>,
                    name: &[u8],
                    f: &mut PushBody<'_>|
         -> Result<(), Box<dyn std::error::Error>> {
            let mark = prefix.len();
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(name);
            let r = f(prefix);
            prefix.truncate(mark);
            r
        };
        match cmp {
            std::cmp::Ordering::Less => {
                let o = old_entries[i].clone();
                push(prefix, o.name.as_slice(), &mut |p| {
                    handle_side(repo, p, &o, true, emit)
                })?;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                let n = new_entries[j].clone();
                push(prefix, n.name.as_slice(), &mut |p| {
                    handle_side(repo, p, &n, false, emit)
                })?;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                let o = old_entries[i].clone();
                let n = new_entries[j].clone();
                if o.oid == n.oid && o.mode == n.mode {
                    i += 1;
                    j += 1;
                    continue;
                }
                push(prefix, n.name.as_slice(), &mut |p| {
                    let o_tree = o.mode.object_kind() == ObjectKind::Tree;
                    let n_tree = n.mode.object_kind() == ObjectKind::Tree;
                    if o_tree && n_tree {
                        tree_diff(repo, Some(o.oid), Some(n.oid), p, emit)
                    } else if o_tree {
                        // tree became something else: emit old's leaves + new leaf
                        tree_diff(repo, Some(o.oid), None, p, emit)?;
                        emit(
                            p,
                            None,
                            Some(TreeFile {
                                oid: n.oid,
                                mode: n.mode.value(),
                            }),
                        )
                    } else if n_tree {
                        emit(
                            p,
                            Some(TreeFile {
                                oid: o.oid,
                                mode: o.mode.value(),
                            }),
                            None,
                        )?;
                        tree_diff(repo, None, Some(n.oid), p, emit)
                    } else {
                        emit(
                            p,
                            Some(TreeFile {
                                oid: o.oid,
                                mode: o.mode.value(),
                            }),
                            Some(TreeFile {
                                oid: n.oid,
                                mode: n.mode.value(),
                            }),
                        )
                    }
                })?;
                i += 1;
                j += 1;
            }
        }
    }
    Ok(())
}

/// For an entry that exists on only one side (pure add or pure delete),
/// emit its leaves: if it's a tree, recurse into it with the other side
/// absent; otherwise emit the single leaf.
fn handle_side(
    repo: &Repository,
    prefix: &mut Vec<u8>,
    entry: &alt_git_codec::TreeEntry,
    deleted: bool,
    emit: &mut TreeEmit<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    if entry.mode.object_kind() == ObjectKind::Tree {
        let (old, new) = if deleted {
            (Some(entry.oid), None)
        } else {
            (None, Some(entry.oid))
        };
        return tree_diff(repo, old, new, prefix, emit);
    }
    let leaf = TreeFile {
        oid: entry.oid,
        mode: entry.mode.value(),
    };
    if deleted {
        emit(prefix, Some(leaf), None)
    } else {
        emit(prefix, None, Some(leaf))
    }
}

/// Walks one commit + its first parent and writes the differences as a
/// stream of `diff --git` stanzas. Binary blobs land as a compact
/// chunk + perceptual summary so a single 100 MiB image doesn't paste a
/// megabyte of header noise (or worse, the bytes) into the terminal.
///
/// Tree walk is lockstep (`tree_diff`): equal-oid subtrees are pruned
/// at every level, so a commit that touched 1-3 files in a 50k-file
/// monorepo reads ~10 trees, not 20 000.
fn emit_patch_for_commit(
    out: &mut impl Write,
    repo: &Repository,
    commit: &ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    let obj = repo
        .read_object(commit)?
        .ok_or_else(|| format!("commit {commit} missing"))?;
    let parsed = Commit::parse(&obj.data)?;
    let new_tree = parsed
        .tree()
        .ok_or_else(|| format!("commit {commit} has no tree header"))?;
    let old_tree = match parsed.parents().next() {
        Some(p) => {
            let p_obj = repo
                .read_object(&p)?
                .ok_or_else(|| format!("commit {p} missing"))?;
            Commit::parse(&p_obj.data)?.tree()
        }
        None => None,
    };

    // Collect diffs from tree_diff into a Vec so we can emit stanzas with
    // the same borrow shape as before. The intermediate is tiny: O(changed
    // leaves), not O(whole tree).
    let mut changes: Vec<(BString, Option<TreeFile>, Option<TreeFile>)> = Vec::new();
    {
        let mut prefix = Vec::new();
        let mut emit_fn = |path: &[u8],
                           o: Option<TreeFile>,
                           n: Option<TreeFile>|
         -> Result<(), Box<dyn std::error::Error>> {
            changes.push((BString::from(path.to_vec()), o, n));
            Ok(())
        };
        tree_diff(repo, old_tree, Some(new_tree), &mut prefix, &mut emit_fn)?;
    }
    changes.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, old, new) in &changes {
        emit_file_stanza(out, repo, path, old.as_ref(), new.as_ref())?;
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
