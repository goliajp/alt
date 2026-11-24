//! What a rebase is to replay, and the editor's part in it: the list of
//! steps and the messages a step asks for.

use super::super::sequence_editor::{self as ed, Action};
use super::super::*;

impl NativeRepo<'_> {
    /// Lists the commits the rebase will replay, oldest first: those reachable
    /// from `orig` but not from `onto`, without merges (git drops them too),
    /// parents before children, older committer time first among peers.
    pub(super) fn commits_to_replay(&self, orig: ObjectId, onto: ObjectId) -> Res<Vec<ObjectId>> {
        let from_orig = self.ancestry(&[orig])?;
        let from_onto = self.ancestry(&[onto])?;
        let mine: std::collections::HashMap<ObjectId, Vec<ObjectId>> = from_orig
            .into_iter()
            .filter(|(c, _)| !from_onto.contains_key(c))
            .collect();
        let mut done = std::collections::HashSet::new();
        let mut order = Vec::new();
        while done.len() < mine.len() {
            let mut ready: Vec<(i64, ObjectId)> = Vec::new();
            for (c, parents) in &mine {
                if !done.contains(c)
                    && parents
                        .iter()
                        .all(|p| !mine.contains_key(p) || done.contains(p))
                {
                    ready.push((self.committer_time(*c)?, *c));
                }
            }
            ready.sort();
            let (_, next) = ready[0];
            done.insert(next);
            if mine[&next].len() < 2 {
                order.push(next);
            }
        }
        Ok(order)
    }

    pub(super) fn edit_todo(
        &mut self,
        todo: &[(Action, ObjectId)],
        onto: ObjectId,
        orig: ObjectId,
    ) -> Res<Vec<(Action, ObjectId)>> {
        let mut text = String::new();
        for (action, c) in todo {
            let subject = self.read_commit_parts(*c)?.subject();
            text.push_str(&format!(
                "{} {} {subject}\n",
                action.word(),
                &c.to_string()[..7]
            ));
        }
        let (o, h) = (&onto.to_string()[..7], &orig.to_string()[..7]);
        text.push_str(&format!(
            "\n# Rebase {o}..{h} onto {o} ({} command{})\n",
            todo.len(),
            if todo.len() == 1 { "" } else { "s" }
        ));
        text.push_str(ed::TODO_HELP);
        ed::check_machine_mode()?;
        let path = self.index_path.with_file_name("REBASE_TODO");
        crate::durable::write(&path, text)?;
        ed::edit(&path, true)?;
        let edited = std::fs::read_to_string(&path)?;
        crate::durable::remove_file(&path)?;
        let repo = self.store.git_view()?;
        ed::parse_todo(&edited, |rev| {
            repo.rev_parse(rev)?
                .ok_or_else(|| format!("rebase: unknown commit '{rev}' in the todo list").into())
        })
    }

    pub(super) fn squash_message(&self, first: &str, next: &str) -> Res<String> {
        let text = format!(
            "# This is a combination of 2 commits.\n# This is the 1st commit message:\n\n{first}\n\
             # This is the commit message #2:\n\n{next}"
        );
        self.edited_message(&text)
    }

    /// Runs the editor on a commit message and applies git's cleanup.
    pub(super) fn edited_message(&self, message: &str) -> Res<String> {
        ed::check_machine_mode()?;
        let path = self.index_path.with_file_name("REBASE_MSG");
        crate::durable::write(&path, message)?;
        ed::edit(&path, false)?;
        let edited = ed::cleanup(&std::fs::read_to_string(&path)?);
        crate::durable::remove_file(&path)?;
        if edited.is_empty() {
            return Err("rebase: aborting because the commit message is empty".into());
        }
        Ok(edited)
    }
}
