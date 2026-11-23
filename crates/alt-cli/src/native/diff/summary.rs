//! What a diff prints in place of the patch: the names of the files that
//! changed, each with how it changed, or how much changed in each.

use super::between::Summary;
use super::worktree::Worktree;
use super::*;
use crate::diffstat::{self, Change, FileStat};

impl NativeRepo<'_> {
    pub(super) fn summarize(
        &self,
        pairs: &[Pair<'_>],
        disk: Option<&Worktree>,
        summary: Summary,
        style: &Style<'_>,
        out: &mut impl Write,
    ) -> Res<()> {
        if summary == Summary::Stat {
            let mut files = Vec::with_capacity(pairs.len());
            for pair in pairs {
                files.push(self.file_stat(pair, disk, style)?);
            }
            return Ok(out.write_all(&diffstat::render(&files))?);
        }
        for pair in pairs {
            let renamed = pair.old_path != pair.new_path;
            if summary == Summary::NameStatus {
                let kind = |w: &WorkEntry| w.mode & 0o170000;
                let letter = match (pair.old, pair.new) {
                    (None, _) => "A".to_owned(),
                    (_, None) => "D".to_owned(),
                    _ if renamed => format!("R{:03}", pair.similarity.unwrap_or(100)),
                    (Some(old), Some(new)) if kind(old) != kind(new) => "T".to_owned(),
                    _ => "M".to_owned(),
                };
                write!(out, "{letter}\t")?;
                if renamed {
                    out.write_all(&style.name("", pair.old_path))?;
                    out.write_all(b"\t")?;
                }
            }
            out.write_all(&style.name("", pair.new_path))?;
            out.write_all(b"\n")?;
        }
        Ok(())
    }

    /// How much one file changed: lines added and removed, or the size of
    /// a binary file before and after.
    fn file_stat(
        &self,
        pair: &Pair<'_>,
        disk: Option<&Worktree>,
        style: &Style<'_>,
    ) -> Res<FileStat> {
        let name = match pair.old_path == pair.new_path {
            true => style.name("", pair.new_path),
            false => diffstat::renamed(pair.old_path, pair.new_path),
        };
        let same = |old: &WorkEntry, new: &WorkEntry| old.oid == new.oid && disk.is_none();
        if let (Some(old), Some(new)) = (pair.old, pair.new)
            && same(old, new)
        {
            let change = Change::Lines(0, 0);
            return Ok(FileStat { name, change });
        }
        let old = self.side_bytes(pair.old, None)?;
        let new = self.side_bytes(pair.new, disk)?;
        let regular = |w: &&WorkEntry| w.mode & 0o170000 == 0o100000;
        let paths = [
            pair.old.filter(regular).map(|_| pair.old_path.as_bytes()),
            pair.new.filter(regular).map(|_| pair.new_path.as_bytes()),
        ];
        if style.sections.binary(paths, &old, &new)? {
            let change = Change::Binary(old.len(), new.len());
            return Ok(FileStat { name, change });
        }
        let (mut added, mut removed) = (0, 0);
        for hunk in alt_diff::hunks(&old, &new, 0) {
            added += hunk.lines.iter().filter(|(tag, _)| *tag == b'+').count();
            removed += hunk.lines.iter().filter(|(tag, _)| *tag == b'-').count();
        }
        let change = Change::Lines(added, removed);
        Ok(FileStat { name, change })
    }
}
