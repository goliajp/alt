//! `alt stash`: what is not committed put away as commits, and brought
//! back.
//!
//! An entry is the commit git makes of one: its tree is the working tree,
//! its first parent the commit HEAD was at, its second a commit of the
//! index, and its third, where untracked files were put away, a commit of
//! those. Each entry has a ref of its own, `refs/stashes/<n>`, a later
//! one a higher number, so that the stack is in the refs and changes by
//! ref transactions alone; `refs/stash` names the latest, as in git.

use super::*;

mod among;
mod apply;
mod save;

pub use among::StashPush;

use alt_export::stash::{ENTRIES, TOP, entry_number};

/// How a stash command says what it did.
#[derive(Clone, Copy, Default)]
pub struct Stashing {
    pub quiet: bool,
    pub json: bool,
}

/// One entry of the stack.
#[derive(Clone, Copy)]
struct Entry {
    number: u64,
    commit: ObjectId,
}

impl Entry {
    fn name(self) -> String {
        format!("{ENTRIES}{}", self.number)
    }
}

/// Which entry a word names: `stash@{2}`, `refs/stash@{2}` or `2`. The
/// number, and what git calls the stack where it finds the number too high.
fn numbered(given: &str) -> Option<(usize, &'static str)> {
    let digits = |text: &str| text.bytes().all(|b| b.is_ascii_digit()) && !text.is_empty();
    if digits(given) {
        return Some((given.parse().ok()?, "refs/stash"));
    }
    let inner = |prefix: &str| given.strip_prefix(prefix)?.strip_suffix('}');
    let at = inner("stash@{").or_else(|| inner("refs/stash@{"))?;
    digits(at)
        .then(|| at.parse().ok())
        .flatten()
        .map(|n| (n, "stash"))
}

impl NativeRepo<'_> {
    /// The entries, the latest first.
    fn stash_entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = Vec::new();
        for (name, target) in self.store.refs.iter() {
            if let (Some(number), RefTarget::Oid(commit)) = (entry_number(name), target) {
                entries.push(Entry {
                    number,
                    commit: *commit,
                });
            }
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.number));
        entries
    }

    /// The entry `named` names, the latest where nothing is named: where
    /// it is in the stack, and what the command calls it.
    fn stash_entry(&self, named: Option<&str>) -> Res<(usize, Entry, String)> {
        let entries = self.stash_entries();
        let (at, stack) = match named {
            None => (0, "refs/stash"),
            Some(given) => numbered(given).ok_or_else(|| {
                said::refused(&format!("error: {given} is not a valid reference\n"), 1)
            })?,
        };
        if entries.is_empty() {
            return Err(said::refused("No stash entries found.\n", 128));
        }
        let entry = entries.get(at).ok_or_else(|| {
            let has = entries.len();
            settings::fatal(format!("log for '{stack}' only has {has} entries"))
        })?;
        let called = match named {
            None => "refs/stash@{0}".to_owned(),
            Some(_) => format!("stash@{{{at}}}"),
        };
        Ok((at, *entry, called))
    }

    /// The ref changes that take the entry at `at` out of the stack.
    fn stash_removal(&self, at: usize) -> Vec<RefChange> {
        let entries = self.stash_entries();
        let entry = entries[at];
        let mut changes = vec![RefChange {
            name: entry.name(),
            old: Some(RefTarget::Oid(entry.commit)),
            new: None,
        }];
        if at == 0 {
            changes.push(RefChange {
                name: TOP.to_owned(),
                old: self.store.refs.get(TOP).cloned(),
                new: entries.get(1).map(|next| RefTarget::Oid(next.commit)),
            });
        }
        changes
    }

    /// `alt stash list`: `stash@{n}: <subject>` for each entry.
    pub fn stash_list(&self, json: bool, out: &mut impl Write) -> Res<()> {
        use crate::json::Json;
        let mut listed = Vec::new();
        for (at, entry) in self.stash_entries().into_iter().enumerate() {
            let subject = self.subject_of(entry.commit)?;
            if json {
                listed.push(Json::Object(vec![
                    ("index", Json::Num(at as i64)),
                    ("commit", Json::str(entry.commit.to_string())),
                    ("message", Json::str(&subject)),
                ]));
            } else {
                writeln!(out, "stash@{{{at}}}: {subject}")?;
            }
        }
        if json {
            crate::json::emit(out, vec![("entries", Json::Array(listed))])?;
        }
        Ok(())
    }

    /// `alt stash show [-p] [<entry>]`: what the entry changed in the
    /// tracked files since the commit it was made on, as a summary or as
    /// the patch.
    pub fn stash_show(
        &self,
        named: Option<&str>,
        patch: bool,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let (_, entry, _) = self.stash_entry(named)?;
        let parents = self.commit_parents(entry.commit)?;
        let base = parents.first().ok_or("a stashed commit has no parent")?;
        let revs = [base.to_string(), entry.commit.to_string()];
        let request = DiffRequest {
            words: crate::rev_or_path::Words {
                before: &revs,
                dashed: true,
                after: &[],
            },
            json,
            summary: (!patch && !json).then_some(Summary::Stat),
            ..Default::default()
        };
        self.diff(&request, out)
    }

    /// `alt stash drop [<entry>]`: the entry out of the stack.
    pub fn stash_drop(
        &mut self,
        named: Option<&str>,
        said: Stashing,
        out: &mut impl Write,
    ) -> Res<()> {
        let (at, entry, called) = self.stash_entry(named)?;
        let changes = self.stash_removal(at);
        self.commit_refs_only("stash", &changes)?;
        self.say_dropped(&called, entry, said, out)
    }

    fn say_dropped(
        &self,
        called: &str,
        entry: Entry,
        said: Stashing,
        out: &mut impl Write,
    ) -> Res<()> {
        if said.json {
            use crate::json::Json;
            let dropped = Json::str(entry.commit.to_string());
            crate::json::emit(out, vec![("dropped", dropped)])?;
        } else if !said.quiet {
            writeln!(out, "Dropped {called} ({})", entry.commit)?;
        }
        Ok(())
    }

    /// `alt stash clear`: every entry out of the stack.
    pub fn stash_clear(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        let entries = self.stash_entries();
        let mut changes: Vec<RefChange> = entries
            .iter()
            .map(|entry| RefChange {
                name: entry.name(),
                old: Some(RefTarget::Oid(entry.commit)),
                new: None,
            })
            .collect();
        if let Some(top) = self.store.refs.get(TOP).cloned() {
            changes.push(RefChange {
                name: TOP.to_owned(),
                old: Some(top),
                new: None,
            });
        }
        if !changes.is_empty() {
            self.commit_refs_only("stash", &changes)?;
        }
        if json {
            use crate::json::Json;
            crate::json::emit(out, vec![("cleared", Json::Num(entries.len() as i64))])?;
        }
        Ok(())
    }

    /// Refuses a stash where the index holds a conflict or an operation
    /// is under way.
    fn ensure_stashable(&self, index: &Index) -> Res<()> {
        if let Some(unmerged) = index.entries.iter().find(|e| e.stage() > 0) {
            let said = format!(
                "{}: needs merge\nerror: you need to resolve your current index first\n",
                unmerged.path
            );
            return Err(said::refused(&said, 1));
        }
        self.ensure_no_merge_in_progress("stash")?;
        if self.cherry_pick_in_progress()? {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "stash: a cherry-pick is in progress; conclude it with `alt cherry-pick \
                 --continue` or give it up with `alt cherry-pick --abort` first",
            )
            .into());
        }
        Ok(())
    }
}
