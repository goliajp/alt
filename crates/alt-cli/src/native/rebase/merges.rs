//! The steps of a rebase that keeps merges (`-r`): each line of commits
//! replayed from where it starts, named where a merge or another line
//! needs it, and the merge commits made again of them, as git lists them.

use std::collections::HashSet;

use super::super::sequence_editor::Action;
use super::super::*;
use super::plan::Own;
use super::state::Step;

/// A line of commits along first parents, oldest first, and the commit
/// it starts on.
type Line = (ObjectId, Vec<ObjectId>);

/// The lines that make up `mine`, each after the lines its merges bring
/// in and the line it starts on: those that end at `tips`, in turn.
fn lines(mine: &Own, tips: &[ObjectId]) -> Vec<Line> {
    fn line(mine: &Own, tip: ObjectId, shown: &mut HashSet<ObjectId>, lines: &mut Vec<Line>) {
        loop {
            let (mut at, mut found) = (tip, Vec::new());
            while mine.contains_key(&at) && !shown.contains(&at) {
                found.push(at);
                at = mine[&at][0];
            }
            let brought = found.iter().rev().filter_map(|c| mine[c].get(1));
            let waits = |b: &&ObjectId| mine.contains_key(*b) && !shown.contains(*b);
            if let Some(&first) = brought.clone().find(waits) {
                // what a merge of this line brings in is replayed before it
                line(mine, first, shown, lines);
                continue;
            }
            if !found.is_empty() {
                shown.extend(&found);
                found.reverse();
                lines.push((at, found));
            }
            return;
        }
    }
    let (mut shown, mut found) = (HashSet::new(), Vec::new());
    for &tip in tips {
        line(mine, tip, &mut shown, &mut found);
    }
    found
}

/// The first of `name`, `name-2`, `name-3`… that is not `taken`, which
/// then is.
fn free(name: &str, taken: &mut HashSet<String>) -> String {
    let mut n = 1;
    loop {
        let tried = match n {
            1 => name.to_owned(),
            n => format!("{name}-{n}"),
        };
        if taken.insert(tried.clone()) {
            return tried;
        }
        n += 1;
    }
}

impl NativeRepo<'_> {
    /// The steps that replay onto `onto` the commits of `orig` that are
    /// not on it, with their merges.
    pub(super) fn steps_with_merges(&self, orig: ObjectId, onto: ObjectId) -> Res<Vec<Step>> {
        let (order, mine) = self.own_commits(orig, onto)?;
        // what each merge brings in, as a walk from the newest finds them
        let mut tips: Vec<(ObjectId, ObjectId)> = Vec::new();
        for commit in order.iter().rev() {
            let short = &commit.to_string()[..7];
            match mine[commit][..] {
                [] => return Err(format!("rebase: {short} has no parent; --rebase-merges does not replay a root commit").into()),
                [_] => {}
                [_, brought] => {
                    if mine.contains_key(&brought) && tips.iter().all(|tip| tip.0 != brought) {
                        tips.push((brought, *commit));
                    }
                }
                _ => return Err(format!("rebase: {short} merges more than two commits; --rebase-merges does not make such a merge again").into()),
            }
        }
        let ends: Vec<ObjectId> = tips.iter().rev().map(|tip| tip.0).chain([orig]).collect();
        let lines = lines(&mine, &ends);
        // a line that starts on the base of the branch goes onto the new
        // base; of several bases that is the one committed last, as by git
        let mut base: Option<(i64, ObjectId)> = None;
        for found in self.merge_bases(&[orig], &[onto])? {
            let at = self.committer_time(found)?;
            if base.is_none_or(|(latest, _)| at > latest) {
                base = Some((at, found));
            }
        }
        let starts: HashSet<ObjectId> = lines.iter().map(|line| line.0).collect();
        let mut taken = HashSet::from(["onto".to_owned()]);
        // the base is the new base wherever it is named: where a line
        // starts on it, and where a merge brought it in
        let mut names: std::collections::HashMap<ObjectId, String> = base
            .map(|(_, commit)| (commit, "onto".to_owned()))
            .into_iter()
            .collect();
        let mut steps = vec![Step::Label("onto".to_owned())];
        for (start, line) in lines {
            let named = names.get(&start).cloned();
            steps.push(Step::Reset(named.unwrap_or_else(|| start.to_string())));
            for commit in line {
                steps.push(match mine[&commit][..] {
                    [_, brought] => {
                        let named = names.get(&brought).cloned();
                        Step::Merge(commit, vec![named.unwrap_or_else(|| brought.to_string())])
                    }
                    _ => Step::Replay(Action::Pick, commit),
                });
                let merged = tips.iter().find(|tip| tip.0 == commit);
                let name = match merged {
                    Some(&(_, by)) => self.name_brought(commit, by)?,
                    None if starts.contains(&commit) => "branch-point".to_owned(),
                    None => continue,
                };
                let name = free(&name, &mut taken);
                names.insert(commit, name.clone());
                steps.push(Step::Label(name));
            }
        }
        Ok(steps)
    }

    /// A name for `commit`, which the merge commit `by` brings in: that of
    /// a branch on it, or what the merge calls it.
    fn name_brought(&self, commit: ObjectId, by: ObjectId) -> Res<String> {
        let on = RefTarget::Oid(commit);
        let mut branches = self.store.refs.iter().filter(|(_, at)| **at == on);
        let branch = branches.find_map(|(name, _)| name.strip_prefix("refs/heads/"));
        let subject = self.read_commit_parts(by)?.subject();
        let quoted = subject
            .strip_prefix("Merge ")
            .and_then(|rest| rest.split('\'').nth(1));
        let name = branch.or(quoted).unwrap_or(&subject);
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/');
        Ok(name
            .chars()
            .map(|c| if plain(c) { c } else { '-' })
            .collect())
    }
}
