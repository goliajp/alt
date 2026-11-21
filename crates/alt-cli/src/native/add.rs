//! `alt add`: stage working-tree files into the index.

use alt_worktree::{Delta, FileStat, Snapshot, Untracked};

use super::*;

mod embedded;
mod lost;
mod named;
mod plan;
mod store;

use pathspec::Spec;
use plan::{Plan, plan, present};

/// How an `add` goes about it.
#[derive(Clone, Copy, Default)]
struct Mode {
    /// every tracked file among the paths is stored again, nothing else
    renormalize: bool,
    /// what an ignore rule hides is staged as well
    force: bool,
    /// only files the index has are staged
    update: bool,
}

impl Mode {
    /// What is not tracked is not looked for where it is not staged, and
    /// then no ignore rule is read.
    fn untracked(self) -> Untracked {
        match (self.renormalize || self.update, self.force) {
            (true, _) => Untracked::No,
            (false, true) => Untracked::Hidden,
            (false, false) => Untracked::All,
        }
    }
}

/// What a scan found among the paths given to an `add`.
struct Found {
    delta: Delta,
    clock: alt_worktree::Clock,
    /// the paths left alone because an ignore rule hides them
    hidden: Vec<BString>,
    /// the converter the scan read files with, if it read any
    attrs: Option<convert::Converter>,
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
    /// Without a path nothing is staged, unless `-u` or `-A` asks for the
    /// whole tree: `-u` stages what changed in the files the index has
    /// and no new file.
    ///
    /// With `--renormalize`, as in git: every tracked file among the
    /// paths is stored again the way the attributes and settings now in
    /// force clean it, whatever the index holds for it. A file that kept
    /// its stat is not looked at otherwise, so a change of the rules
    /// alone reaches the index only this way. Untracked files, conflicted
    /// paths and submodules are left as they are, and a tracked file that
    /// is gone is an error.
    pub fn add_paths(&mut self, args: &crate::worktree_args::Add, out: &mut impl Write) -> Res<()> {
        if args.update && args.all {
            let both = "options '-A' and '-u' cannot be used together";
            return Err(settings::fatal(both.into()));
        }
        let whole = [":/".to_owned()];
        let paths = match args.paths.is_empty() && (args.update || args.all) {
            true => &whole[..],
            false => &args.paths[..],
        };
        if paths.is_empty() {
            let said = named::nothing_specified(&self.settings()?)?;
            crate::diagnostics::emit(said.as_bytes())?;
            return Ok(());
        }
        let mode = Mode {
            renormalize: args.renormalize,
            force: args.forced,
            update: args.update,
        };
        self.stage(paths, mode, args.json, out)
    }

    fn stage(&mut self, paths: &[String], mode: Mode, json: bool, out: &mut impl Write) -> Res<()> {
        let before = self.begin_add(paths, mode.update)?;
        let parse = |given: &String| Spec::parse(&self.root, &self.prefix, given);
        let specs: Vec<Spec> = paths.iter().map(parse).collect::<Res<_>>()?;
        self.ensure_reachable(&specs, &before)?;
        // only what differs from the index is read: a file with the stat
        // it had when it was staged holds what the index holds
        let mut snapshot = self.worktree_snapshot()?;
        let found = self.find(&specs, &before, mode, &mut snapshot)?;
        let (mut delta, hidden) = (found.delta, found.hidden);
        let given = pathspec::Named::of(&specs);
        let named = |path: &[u8]| given.has(path);
        let embedded = self.take_embedded(&mut delta, &named)?;
        let advice = embedded::warning(&embedded, &self.settings()?)?;

        // a staged file whose blob went with records that were never
        // synced is stored again, though its stat is the one it was staged with
        let unstored = self.staged_unstored(&mut snapshot, &before, &named)?;
        let plan = plan(&before, &delta, &unstored.lost, mode.renormalize, &named);
        self.ensure_paths_allowed(&before, &plan, &delta, &named)?;
        // stored anew, a file is cleaned without regard to what is staged
        let none = index_of(Vec::new());
        let staged_now = if mode.renormalize { &none } else { &*before };
        let mut entries = plan.entries;
        let store::Stored { stats, blocked } =
            self.stage_files(&plan.taken, &delta, staged_now, &mut entries, found.attrs)?;
        // what could not be stored stays as it was staged
        let kept = before.entries.iter().filter(|e| blocked.contains(&e.path));
        entries.extend(kept.cloned());
        self.store.odb.flush()?;
        entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.stage().cmp(&b.stage())));
        let after = index_of(entries);
        let staged = plan.staged;
        let touched = plan.touched;
        let restaged = self.lost_stop_restored().then(|| after.clone());
        // the file the index was read from, to be noted where it stays
        let read = before.file().filter(|_| unstored.none.is_some()).cloned();
        let replaced = self.put_staged(before, after, &touched, snapshot, stats, &found.clock)?;
        if let Some(at) = unstored.none {
            self.note_stored(read.as_ref(), replaced, at);
        }
        self.keep_restaged(restaged)?;
        if json {
            crate::json::emit(out, vec![("staged", crate::json::Json::Num(staged as i64))])?;
        } else {
            if !hidden.is_empty() {
                crate::diagnostics::emit(&named::ignored(&hidden, &self.settings()?)?)?;
            }
            crate::diagnostics::emit(&advice)?;
            writeln!(out, "staged {staged} path(s)")?;
        }
        named::ended(&hidden, !blocked.is_empty())
    }

    /// Scans where the `specs` reach and keeps what they name of it. A
    /// path that names nothing ends the command, as for git.
    fn find(
        &self,
        specs: &[Spec],
        before: &Index,
        mode: Mode,
        snapshot: &mut Snapshot,
    ) -> Res<Found> {
        let reach = specs.iter().filter(|spec| !spec.exclude);
        let reach: Vec<&[u8]> = reach.map(|spec| spec.reach().as_bytes()).collect();
        let within = match reach.iter().any(|dir| dir.is_empty()) {
            true => &[][..],
            false => &reach[..],
        };
        let tracked_only = mode.renormalize || mode.update;
        if !tracked_only {
            named::ensure_walkable(specs)?;
        }
        let (mut delta, clock, attrs) =
            self.scan_delta_within(snapshot, mode.untracked(), within)?;
        let known = match (mode.renormalize, mode.update) {
            (true, _) => named::Known::Tracked,
            (false, true) => named::Known::TrackedOrError,
            (false, false) => named::Known::Any,
        };
        self.ensure_matched(specs, before, &delta, known)?;
        let hidden = match tracked_only || mode.force {
            true => Vec::new(),
            false => self.hidden(specs, before)?,
        };
        // the scan looked at more than is named where a path has wildcards
        let given = pathspec::Named::of(specs);
        let named = |path: &[u8]| given.has(path);
        delta.changed.retain(|w| named(&w.path));
        delta.deleted.retain(|path| named(path));
        delta.unmerged.retain(|path| named(path));
        pathspec::keep_named(specs, &mut delta.untracked);
        if mode.update && !mode.renormalize {
            self.take_replaced(&mut delta)?;
        }
        Ok(Found {
            delta,
            clock,
            hidden,
            attrs,
        })
    }

    /// The path gate: denies staging any path the policy excludes, a removal
    /// as well, before a blob is written to the object store, so a denial
    /// has no side effect.
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
        // a removal is staged as much as a file is: every path whose
        // entries change is asked for, beside the files that stay
        let present = |e: &&IndexEntry| present(e, delta, named);
        let kept = before.entries.iter().filter(present).map(|e| &e.path);
        for path in kept.chain(&plan.touched) {
            self.ensure_path_allowed(path)?;
        }
        Ok(())
    }

    /// Puts the index `after` in place of `before` where they differ, and
    /// keeps the snapshot of what is then in place, with the stat each
    /// staged file had before it was read. `snapshot` is the one the scan
    /// went by. Returns whether the index was replaced.
    fn put_staged(
        &mut self,
        before: recovery::AsRead,
        after: Index,
        touched: &[BString],
        mut snapshot: Snapshot,
        stats: Vec<(BString, FileStat)>,
        clock: &alt_worktree::Clock,
    ) -> Res<bool> {
        if after.entries == before.entries {
            self.keep_snapshot(&mut snapshot);
            return Ok(false);
        }
        let algo = self.store.algo()?;
        // a snapshot of the index that is replaced becomes the one of the
        // new index at the paths that differ; any other is made anew
        let changes = before
            .ends_in(&snapshot.index_id)
            .then(|| replaced(&after, touched));
        let anew = match changes {
            Some(_) => None,
            None => Some(Snapshot::of_index(&after, &[], algo, Some(&snapshot))),
        };
        self.write_index_transaction(before, after, touched, None)?;
        let id = tree_state::index_checksum(&self.index_path, algo.raw_len())?.unwrap_or_default();
        let mut next = match (changes, anew) {
            (Some(changes), _) => {
                snapshot.replace_paths(&id, &changes);
                snapshot
            }
            (None, anew) => {
                let mut anew = anew.expect("made where there are no changes to go by");
                anew.index_id = id;
                anew
            }
        };
        for (path, stat) in stats {
            next.settle_path(&path, stat, clock);
        }
        next.fill_trees();
        self.keep_snapshot(&mut next);
        Ok(true)
    }
}

/// What `after` has at each of the paths `touched`, which are in order.
fn replaced<'a>(after: &Index, touched: &'a [BString]) -> Vec<alt_worktree::Replaced<'a>> {
    let entries = &after.entries;
    let at = |path: &'a BString| {
        let from = entries.partition_point(|e| e.path < *path);
        let there = entries[from..].iter().take_while(|e| e.path == *path);
        alt_worktree::Replaced {
            path: path.as_slice(),
            entries: there.map(|e| (e.mode, e.flags, e.oid)).collect(),
        }
    };
    touched.iter().map(at).collect()
}

fn index_of(entries: Vec<IndexEntry>) -> Index {
    Index {
        version: 2,
        entries,
        extensions: Vec::new(),
    }
}
