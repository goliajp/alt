//! The patch of a commit as `log -p` and `show` print it: one `diff --git`
//! stanza per changed file against the first parent, renames paired, in
//! git's layout. A merge commit prints no patch.

use std::io::Write;

use alt_bytes::ByteSlice;
use alt_git_codec::{Commit, ObjectId};
use alt_repo::Repository;

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
    let abbrev = abbrev_len(repo.object_count().unwrap_or(0));
    for file in tree_changes::pair_renames(repo, changes)? {
        let kind = |f: &TreeFile| f.mode & 0o170000;
        match (&file.old, &file.new) {
            // a path that changed kind is a removal, then an addition
            (Some(old), Some(new)) if kind(old) != kind(new) => {
                let (removed, added) = split(&file);
                write_file(out, repo, &removed, abbrev)?;
                write_file(out, repo, &added, abbrev)?;
            }
            _ => write_file(out, repo, &file, abbrev)?,
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

/// How many hex digits git shows of an object id in a store of `objects`
/// objects: enough for ids to stay apart at that size, and never fewer
/// than seven. Ids that still collide at that length are not lengthened.
pub(crate) fn abbrev_len(objects: usize) -> usize {
    let bits = (usize::BITS - objects.leading_zeros()) as usize;
    bits.div_ceil(2).max(7)
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

fn write_file(out: &mut impl Write, repo: &Repository, file: &FileDiff, abbrev: usize) -> Res<()> {
    let new_path = file.path.to_str_lossy();
    let old_path = match &file.old_path {
        Some(p) => p.to_str_lossy(),
        None => new_path.clone(),
    };
    writeln!(out, "diff --git a/{old_path} b/{new_path}")?;
    match (&file.old, &file.new) {
        (None, Some(n)) => writeln!(out, "new file mode {:06o}", n.mode)?,
        (Some(o), None) => writeln!(out, "deleted file mode {:06o}", o.mode)?,
        (Some(o), Some(n)) if o.mode != n.mode => {
            writeln!(out, "old mode {:06o}", o.mode)?;
            writeln!(out, "new mode {:06o}", n.mode)?;
        }
        _ => {}
    }
    if let Some(percent) = file.similarity {
        writeln!(out, "similarity index {percent}%")?;
        writeln!(out, "rename from {old_path}")?;
        writeln!(out, "rename to {new_path}")?;
    }
    let (old_oid, new_oid) = (file.old.map(|f| f.oid), file.new.map(|f| f.oid));
    if old_oid == new_oid {
        return Ok(()); // moved or re-moded without a change of content
    }
    let short = |oid: Option<ObjectId>| match oid {
        Some(oid) => oid.to_string()[..abbrev].to_owned(),
        None => "0".repeat(abbrev),
    };
    write!(out, "index {}..{}", short(old_oid), short(new_oid))?;
    match (&file.old, &file.new) {
        (Some(o), Some(n)) if o.mode == n.mode => writeln!(out, " {:06o}", o.mode)?,
        _ => writeln!(out)?,
    }

    let old_bytes = content(repo, file.old.as_ref())?;
    let new_bytes = content(repo, file.new.as_ref())?;
    let old_name = match file.old {
        Some(_) => format!("a/{old_path}"),
        None => "/dev/null".to_owned(),
    };
    let new_name = match file.new {
        Some(_) => format!("b/{new_path}"),
        None => "/dev/null".to_owned(),
    };
    if alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes) {
        writeln!(out, "Binary files {old_name} and {new_name} differ")?;
        return write_binary_summary(out, &old_bytes, &new_bytes);
    }
    let mut hunks = Vec::new();
    if alt_diff::write_unified(&mut hunks, &old_bytes, &new_bytes, 3) {
        writeln!(out, "--- {old_name}")?;
        writeln!(out, "+++ {new_name}")?;
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

#[cfg(test)]
mod tests {
    use super::abbrev_len;

    #[test]
    fn abbreviation_grows_with_the_store() {
        assert_eq!(abbrev_len(0), 7);
        assert_eq!(abbrev_len(10_000), 7);
        // the sizes of the cargo and git repositories, where git prints
        // 9 and 10 digits
        assert_eq!(abbrev_len(185_011), 9);
        assert_eq!(abbrev_len(402_230), 10);
        assert_eq!(abbrev_len(1 << 20), 11);
    }
}
