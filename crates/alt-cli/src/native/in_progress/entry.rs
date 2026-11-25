//! The way back into a revert, a cherry-pick, a rebase or a merge that
//! stopped: `--continue`, `--skip`, `--abort` and `--quit` all pass the
//! one check here before they touch the working tree, and it refuses
//! what git refuses, in git's words.

use std::collections::HashSet;

use super::super::*;

/// The command gone back to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum Sequence {
    Revert,
    CherryPick,
    Rebase,
    Merge,
}

/// The way it is gone back to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum Entry {
    Continue,
    Skip,
    Abort,
    Quit,
}

/// Where the command stands.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum Stands {
    /// it is not under way
    Nowhere,
    /// the step that stopped waits for its commit
    Open,
    /// the step that stopped was concluded, by a commit of the user
    /// or by a refusal, and what follows waits
    Concluded,
}

/// What waits in the index and the files as the command is gone back to.
pub(in crate::native) struct Waiting {
    /// the tracked files changed or gone and not staged so
    pub(in crate::native) unstaged: HashSet<BString>,
    /// whether the index holds what the commit HEAD is at does not
    pub(in crate::native) staged: bool,
}

impl Sequence {
    fn name(self) -> &'static str {
        match self {
            Sequence::Revert => "revert",
            Sequence::CherryPick => "cherry-pick",
            Sequence::Rebase => "rebase",
            Sequence::Merge => "merge",
        }
    }
}

const UNMERGED: &str = "error: Committing is not possible because you have unmerged files.\n\
    hint: Fix them up in the work tree, and then use 'alt add/rm <file>'\n\
    hint: as appropriate to mark resolution and make a commit.\n\
    fatal: Exiting because of an unresolved conflict.\n";

const NOT_RESOLVED: &str =
    "You must edit all merge conflicts and then\nmark them as resolved using git add";

impl NativeRepo<'_> {
    /// What git says where `sequence` is not under way and `entry` asks
    /// for it; `None` where git does nothing and ends well.
    fn nowhere(sequence: Sequence, entry: Entry) -> Option<String> {
        let name = sequence.name();
        Some(match (sequence, entry) {
            (Sequence::Rebase, _) => "fatal: no rebase in progress\n".to_owned(),
            (_, Entry::Quit) => return None,
            (Sequence::Merge, Entry::Abort) => {
                "fatal: There is no merge to abort (MERGE_HEAD missing).\n".to_owned()
            }
            (Sequence::Merge, _) => {
                "fatal: There is no merge in progress (MERGE_HEAD missing).\n".to_owned()
            }
            (_, Entry::Skip) => format!("error: no {name} in progress\nfatal: {name} failed\n"),
            _ => format!("error: no cherry-pick or revert in progress\nfatal: {name} failed\n"),
        })
    }

    /// The check every way back into a stopped command passes before it
    /// touches the index or the working tree. `None` where git ends
    /// there with nothing done: a command that is not under way and is
    /// to be forgotten, which ends well, and a rebase that has said on
    /// standard output why it does not go on.
    pub(in crate::native) fn enter(
        &self,
        sequence: Sequence,
        entry: Entry,
        stands: Stands,
        out: &mut impl Write,
    ) -> Res<Option<Waiting>> {
        let name = sequence.name();
        if stands == Stands::Nowhere {
            return match Self::nowhere(sequence, entry) {
                Some(said) => Err(said::refused(&said, 128)),
                None => Ok(None),
            };
        }
        let index = self.index()?;
        let unmerged = index.entries.iter().any(|e| e.stage() > 0);
        let conflicted = index.entries.iter().filter(|e| e.stage() > 0);
        let conflicted: HashSet<&BString> = conflicted.map(|e| &e.path).collect();
        let unstaged = self.unstaged()?;
        let open = stands == Stands::Open;
        match (sequence, entry) {
            (_, Entry::Quit | Entry::Abort) => {}
            (Sequence::Rebase, Entry::Continue) => {
                if unmerged || !unstaged.is_empty() {
                    if crate::diagnostics::json_mode() {
                        let refusal =
                            crate::machine::Failure::refusal("precondition_failed", NOT_RESOLVED);
                        return Err(refusal.into());
                    }
                    writeln!(out, "{NOT_RESOLVED}")?;
                    return Ok(None);
                }
            }
            (Sequence::Rebase, Entry::Skip) => {
                // git resets hard here and loses what is not staged; the
                // steps that follow may write any file
                if let Some(path) = unstaged.iter().filter(|p| !conflicted.contains(p)).min() {
                    let said = format!(
                        "rebase --skip: '{path}' has changes that are not staged, and the steps \
                         that follow may write over them; put them away with `alt stash`, or \
                         take them back with `alt restore {path}`, and skip again"
                    );
                    let refusal = crate::machine::Failure::refusal("precondition_failed", said);
                    return Err(refusal.into());
                }
            }
            (_, Entry::Continue) if open && unmerged => {
                return Err(said::refused(UNMERGED, 128));
            }
            (Sequence::Merge, _) => {}
            (_, Entry::Continue) if !open && self.index_off_head()? => {
                let said = format!(
                    "error: your local changes would be overwritten by {name}.\n\
                     hint: commit your changes or stash them to proceed.\n\
                     fatal: {name} failed\n"
                );
                return Err(said::refused(&said, 128));
            }
            (_, Entry::Skip) if !open => {
                let said = format!(
                    "error: there is nothing to skip\nhint: have you committed already?\n\
                     hint: try \"alt {name} --continue\"\nfatal: {name} failed\n"
                );
                return Err(said::refused(&said, 128));
            }
            _ => {}
        }
        let staged = self.index_off_head()?;
        Ok(Some(Waiting { unstaged, staged }))
    }

    /// Whether the index holds a tree other than that of the commit
    /// HEAD is at; an index in conflict does.
    fn index_off_head(&self) -> Res<bool> {
        let index = self.index()?;
        if index.entries.iter().any(|e| e.stage() > 0) {
            return Ok(true);
        }
        let head = match self.store.refs.resolve(&self.head_ref)? {
            Some(head) => self.commit_entries(head)?,
            None => Vec::new(),
        };
        let held: Vec<(&BString, ObjectId, u32)> =
            head.iter().map(|e| (&e.path, e.oid, e.mode)).collect();
        let staged: Vec<(&BString, ObjectId, u32)> = index
            .entries
            .iter()
            .map(|e| (&e.path, e.oid, e.mode))
            .collect();
        Ok(held != staged)
    }
}
