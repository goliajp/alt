//! `alt add`: stage working-tree files into the index.

use std::collections::{HashMap, HashSet};

use alt_worktree::{Delta, FileStat, Snapshot, Untracked};

use super::*;

mod embedded;
mod named;
mod plan;

use pathspec::Spec;
use plan::{Plan, plan, present};

/// How an `add` goes about it.
#[derive(Clone, Copy, Default)]
struct Mode {
    /// every tracked file among the paths is stored again, nothing else
    renormalize: bool,
    /// what an ignore rule hides is staged as well
    force: bool,
}

impl NativeRepo<'_> {
    /// `alt add <paths>`: stage the given paths (or everything for `.`),
    /// updating the index to match the working tree.
    pub fn add(&mut self, paths: &[String], json: bool, out: &mut impl Write) -> Res<()> {
        self.stage(paths, Mode::default(), json, out)
    }

    /// `alt add` as the command line has it. Where a path named is one an
    /// ignore rule hides, the others are staged all the same and the
    /// command ends as git does, with status 1 and the paths left alone.
    ///
    /// With `--renormalize`, as in git: every tracked file among the
    /// paths is stored again the way the attributes and settings now in
    /// force clean it, whatever the index holds for it. A file that kept
    /// its stat is not looked at otherwise, so a change of the rules
    /// alone reaches the index only this way. Untracked files, conflicted
    /// paths and submodules are left as they are, and a tracked file that
    /// is gone is an error.
    pub fn add_paths(&mut self, args: &crate::worktree_args::Add, out: &mut impl Write) -> Res<()> {
        if args.renormalize && args.paths.is_empty() {
            writeln!(out, "Nothing specified, nothing added.")?;
            return Ok(());
        }
        let mode = Mode {
            renormalize: args.renormalize,
            force: args.forced,
        };
        self.stage(&args.paths, mode, args.json, out)
    }

    fn stage(&mut self, paths: &[String], mode: Mode, json: bool, out: &mut impl Write) -> Res<()> {
        self.ensure_writable("add")?;
        self.lock_workspace()?;
        self.ensure_no_pending_recovery()?;
        let before = self.index()?;
        let parse = |given: &String| Spec::parse(&self.root, &self.prefix, given);
        let specs: Vec<Spec> = paths.iter().map(parse).collect::<Res<_>>()?;
        self.ensure_reachable(&specs, &before)?;
        let reach: Vec<&[u8]> = specs
            .iter()
            .filter(|spec| !spec.exclude)
            .map(|spec| spec.reach().as_bytes())
            .collect();
        let within = match reach.iter().any(|dir| dir.is_empty()) {
            true => &[][..],
            false => &reach[..],
        };
        let named = |path: &[u8]| pathspec::named(&specs, path);
        let untracked = match mode.force {
            true => Untracked::Hidden,
            false => Untracked::All,
        };

        // only what differs from the index is read: a file with the stat
        // it had when it was staged holds what the index holds
        let mut snapshot = self.worktree_snapshot()?;
        let (mut delta, clock) = self.scan_delta_within(&mut snapshot, untracked, within)?;
        let hidden = self.unmatched(&specs, &before, &delta, mode.renormalize)?;
        // the scan looked at more than is named where a path has wildcards
        delta.changed.retain(|w| named(&w.path));
        for list in [
            &mut delta.deleted,
            &mut delta.untracked,
            &mut delta.unmerged,
        ] {
            list.retain(|path| named(path));
        }
        let embedded = match mode.renormalize {
            true => Vec::new(),
            false => self.take_embedded(&mut delta, &named)?,
        };
        let advice = embedded::warning(&embedded, &self.settings()?)?;

        let plan = plan(&before, &delta, mode.renormalize, &named);
        self.ensure_paths_allowed(&before, &plan, &delta, &named)?;
        // stored anew, a file is cleaned without regard to what is staged
        let none = index_of(Vec::new());
        let staged_now = if mode.renormalize { &none } else { &before };
        let mut entries = plan.entries;
        let stats = self.stage_files(&plan.taken, &delta, staged_now, &mut entries)?;
        self.store.odb.flush()?;
        entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.stage().cmp(&b.stage())));
        let after = index_of(entries);
        let staged = plan.staged;
        let touched = plan.touched;
        self.put_staged(before, after, &touched, &snapshot, stats, &clock)?;
        if json {
            crate::json::emit(out, vec![("staged", crate::json::Json::Num(staged as i64))])?;
        } else {
            crate::diagnostics::emit(advice.as_bytes())?;
            writeln!(out, "staged {staged} path(s)")?;
        }
        if hidden.is_empty() {
            return Ok(());
        }
        let report = named::ignored(&hidden, &self.settings()?)?;
        Err(crate::machine::Failure::told(crate::machine::PATHS_IGNORED, report).into())
    }

    /// The path gate: denies staging any path the policy excludes, before
    /// a blob is written to the object store, so a denial has no side effect.
    fn ensure_paths_allowed(
        &self,
        before: &Index,
        plan: &Plan<'_>,
        delta: &Delta,
        named: &dyn Fn(&[u8]) -> bool,
    ) -> Res<()> {
        if self.caps.path_allow.is_empty() {
            return Ok(());
        }
        let gone: HashSet<&[u8]> = plan.deleted.iter().map(|p| p.as_slice()).collect();
        let stays = |e: &&IndexEntry| !gone.contains(e.path.as_slice());
        let present = |e: &&IndexEntry| present(e, delta, named);
        for e in before.entries.iter().filter(present).filter(stays) {
            self.ensure_path_allowed(&e.path)?;
        }
        Ok(())
    }

    /// Puts the index `after` in place of `before` where they differ, and
    /// keeps the snapshot of what is then in place, with the stat each
    /// staged file had before it was read.
    fn put_staged(
        &mut self,
        before: Index,
        after: Index,
        touched: &[BString],
        snapshot: &Snapshot,
        stats: Vec<(BString, FileStat)>,
        clock: &alt_worktree::Clock,
    ) -> Res<()> {
        if after.entries == before.entries {
            self.keep_snapshot(snapshot);
            return Ok(());
        }
        let mut next = Snapshot::of_index(&after, &[], self.store.algo, Some(snapshot));
        self.write_index_transaction(before, after, touched, None)?;
        let raw = self.store.algo.raw_len();
        next.index_id = tree_state::index_checksum(&self.index_path, raw)?.unwrap_or_default();
        for (path, stat) in stats {
            next.settle_path(&path, stat, clock);
        }
        next.fill_trees();
        self.keep_snapshot(&next);
        Ok(())
    }

    /// Stores what the working tree has at each of `taken` and pushes its
    /// entry; returns the stat each file had before it was read.
    fn stage_files(
        &mut self,
        taken: &[&BString],
        delta: &Delta,
        before: &Index,
        entries: &mut Vec<IndexEntry>,
    ) -> Res<Vec<(BString, FileStat)>> {
        let lfs = self.lfs_rules()?;
        let attrs = self.converter()?;
        let moved: HashMap<&[u8], &WorkEntry> = delta
            .changed
            .iter()
            .filter(|w| w.mode == 0o160000)
            .map(|w| (w.path.as_slice(), w))
            .collect();
        let unread = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, b"");
        let mut stats = Vec::new();
        for &path in taken {
            self.ensure_path_allowed(path)?;
            // a submodule is recorded by the commit it has checked out,
            // which lives in its own store
            if let Some(w) = moved.get(path.as_slice()) {
                entries.push(staged_entry(path, w.oid, w.mode));
                continue;
            }
            let meta = std::fs::symlink_metadata(self.abs(path)?)
                .map_err(|e| format!("unable to stat '{path}': {e}"))?;
            let stat = FileStat::of(&meta);
            // the id is known once the stored form is: not used until then
            let file = WorkEntry {
                path: path.clone(),
                oid: unread,
                mode: stat.git_mode(),
            };
            let bytes = self.staged_bytes(&file, lfs.as_ref(), &attrs, before)?;
            let oid = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, &bytes);
            self.store.odb.put(oid, ObjectKind::Blob, &bytes)?;
            entries.push(staged_entry(path, oid, file.mode));
            stats.push((path.clone(), stat));
        }
        Ok(stats)
    }
}

fn index_of(entries: Vec<IndexEntry>) -> Index {
    Index {
        version: 2,
        entries,
        extensions: Vec::new(),
    }
}

/// An index entry for staged content. The index keeps no stat: what the
/// working-tree file looked like is the snapshot's to remember.
fn staged_entry(path: &BString, oid: ObjectId, mode: u32) -> IndexEntry {
    IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid,
        flags: path.len().min(0x0FFF) as u16,
        extended_flags: None,
        path: path.clone(),
    }
}
