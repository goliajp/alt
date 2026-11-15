//! The patch of a commit as `log -p` and `show` print it: one `diff --git`
//! stanza per changed file against the first parent, renames paired, in
//! git's layout. A merge commit prints no patch.

use std::io::Write;

use alt_git_codec::{Commit, ObjectId};
use alt_repo::Repository;

use crate::file_header::{FileHeader, Side, Style};
use crate::tree_changes::{self, FileDiff, TreeFile};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Writes the patch `commit` makes to its parent, or to the empty tree
/// for a root commit. Nothing is written for a merge: what a merge changes
/// is in the commits it merges.
pub(crate) fn write_commit(out: &mut impl Write, repo: &Repository, commit: &ObjectId) -> Res<()> {
    let parsed = read_commit(repo, commit)?;
    let mut parents = parsed.parents();
    let old_tree = match (parents.next(), parents.next()) {
        (_, Some(_)) => return Ok(()),
        (Some(parent), None) => read_commit(repo, &parent)?.tree(),
        (None, _) => None,
    };
    let new_tree = parsed
        .tree()
        .ok_or_else(|| format!("commit {commit} has no tree header"))?;
    let changes = tree_changes::changes(repo, old_tree, Some(new_tree))?;
    let style = style(repo)?;
    for file in tree_changes::pair_renames(repo, changes)? {
        let kind = |f: &TreeFile| f.mode & 0o170000;
        match (&file.old, &file.new) {
            // a path that changed kind is a removal, then an addition
            (Some(old), Some(new)) if kind(old) != kind(new) => {
                let (removed, added) = split(&file);
                write_file(out, repo, &removed, &style)?;
                write_file(out, repo, &added, &style)?;
            }
            _ => write_file(out, repo, &file, &style)?,
        }
    }
    Ok(())
}

fn read_commit(repo: &Repository, oid: &ObjectId) -> Res<Commit> {
    let obj = repo
        .read_object(oid)?
        .ok_or_else(|| format!("commit {oid} missing"))?;
    Ok(Commit::parse(&obj.data)?)
}

fn split(file: &FileDiff) -> (FileDiff, FileDiff) {
    let half = |old, new| FileDiff {
        path: file.path.clone(),
        old_path: None,
        old,
        new,
        similarity: None,
    };
    (half(file.old, None), half(None, file.new))
}

/// How this repository prints names and ids.
pub(crate) fn style(repo: &Repository) -> Res<Style<'_>> {
    let quotepath = repo.config().get_bool("core", None, "quotepath");
    Ok(Style {
        quotepath: quotepath.transpose()?.unwrap_or(true),
        abbrev: repo.abbrev(),
    })
}

/// What a side of a file is diffed as. A submodule has no content in this
/// store: it is the line git prints for it.
fn content(repo: &Repository, side: Option<&TreeFile>) -> Res<Vec<u8>> {
    match side {
        None => Ok(Vec::new()),
        Some(f) if f.mode == 0o160000 => Ok(format!("Subproject commit {}\n", f.oid).into_bytes()),
        Some(f) => Ok(repo
            .read_object(&f.oid)?
            .ok_or_else(|| format!("blob {} missing", f.oid))?
            .data),
    }
}

fn write_file(
    out: &mut impl Write,
    repo: &Repository,
    file: &FileDiff,
    style: &Style<'_>,
) -> Res<()> {
    let side = |f: &TreeFile| Side {
        oid: f.oid,
        mode: f.mode,
    };
    let header = FileHeader {
        old_path: file.old_path.as_ref().unwrap_or(&file.path),
        new_path: &file.path,
        old: file.old.as_ref().map(side),
        new: file.new.as_ref().map(side),
        similarity: file.similarity,
    };
    if !header.write(out, style)? {
        return Ok(()); // moved or re-moded without a change of content
    }
    let old_bytes = content(repo, file.old.as_ref())?;
    let new_bytes = content(repo, file.new.as_ref())?;
    if alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes) {
        header.write_binary_notice(out, style)?;
        return write_binary_summary(out, &old_bytes, &new_bytes);
    }
    let mut hunks = Vec::new();
    if alt_diff::write_unified(&mut hunks, &old_bytes, &new_bytes, 3) {
        header.write_hunk_names(out, style)?;
        out.write_all(&hunks)?;
    }
    Ok(())
}

/// What stands in for the bytes of a binary change: how much of the
/// content is shared, and for a recognised image kind how far apart the
/// pictures are.
fn write_binary_summary(out: &mut impl Write, old: &[u8], new: &[u8]) -> Res<()> {
    let cd = alt_diff::binary::chunk_diff(old, new, alt_diff::binary::DEFAULT_PARAMS);
    let pct = (cd.byte_shared_ratio() * 100.0).round() as u32;
    writeln!(
        out,
        "chunks: {} shared, {} added, {} removed ({pct}% bytes shared)",
        cd.shared_chunks, cd.added_chunks, cd.removed_chunks,
    )?;
    let old_fp = alt_diff::perceptual::fingerprint(old);
    let new_fp = alt_diff::perceptual::fingerprint(new);
    if let (Some(d), Some(fp)) = (alt_diff::perceptual::distance(old_fp, new_fp), old_fp) {
        let pct_off = (d * 100.0).round() as u32;
        writeln!(
            out,
            "perceptual diff: {pct_off}% off (prism={})",
            fp.kind.as_str()
        )?;
    }
    Ok(())
}
