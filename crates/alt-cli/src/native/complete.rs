//! `alt complete` in a repository: the candidates of the values only the
//! repository knows.

use alt_worktree::Untracked;

use super::*;
use crate::complete::{Answer, Asked, CompleteArgs, answer, group};

mod paths;

/// The most commits offered for the start of an object id.
const COMMITS: usize = 100;

/// The settings alt reads, as a user writes their names.
const KNOWN_KEYS: [&str; 10] = [
    "advice.addEmbeddedRepo",
    "advice.addEmptyPathspec",
    "advice.addIgnoredFile",
    "core.attributesFile",
    "core.autocrlf",
    "core.eol",
    "core.excludesFile",
    "core.quotePath",
    "status.relativePaths",
    "status.showUntrackedFiles",
];

impl NativeRepo<'_> {
    /// `alt complete`: what may stand at the cursor. A value whose
    /// candidates cannot be read is left without any, and the rest of
    /// the answer stands.
    pub fn complete(&self, args: &CompleteArgs, out: &mut impl Write) -> Res<()> {
        let found = answer(args, &mut |asked, answer| {
            let mut own = Answer::default();
            if self.candidates(asked, &mut own).is_ok() {
                answer.text.extend(own.text);
            }
        });
        Ok(out.write_all(&found.text)?)
    }

    fn candidates(&self, asked: &Asked, answer: &mut Answer) -> Res<()> {
        match asked.kind {
            "branch" => answer.listed(group::BRANCHES, &self.refs_under("refs/heads/")),
            "tag" => answer.listed(group::TAGS, &self.refs_under("refs/tags/")),
            "branch-delete" => {
                let remote = |word: &str| word == "--remotes" || word.starts_with("-r");
                match asked.others().any(remote) {
                    true => answer.listed(group::REMOTE_BRANCHES, &self.remote_branches()),
                    false => answer.listed(group::BRANCHES, &self.refs_under("refs/heads/")),
                }
            }
            "rev" => self.revisions(asked.typed, answer)?,
            "remote" => {
                let remotes = self.read_remotes()?;
                if !remotes.is_empty() {
                    answer.group(group::REMOTES);
                }
                for remote in &remotes {
                    answer.candidate(&remote.name, &remote.url);
                }
            }
            "refspec" => self.refspecs(asked, answer)?,
            "workspace" => self.workspaces(answer)?,
            "flow-topic" => {
                let mut words = asked.words.iter().skip_while(|word| *word != "flow");
                let topic = words.nth(1).map_or("", String::as_str);
                if matches!(topic, "feature" | "release" | "hotfix") {
                    let names = self.refs_under(&format!("refs/heads/{topic}/"));
                    answer.listed(group::BRANCHES, &names);
                }
            }
            "config-key" => {
                let settings = self.settings()?;
                let mut keys: Vec<String> =
                    settings.entries.iter().map(|e| e.display_key()).collect();
                let unset = |known: &&str| !keys.iter().any(|key| key.eq_ignore_ascii_case(known));
                let known: Vec<&str> = KNOWN_KEYS.iter().copied().filter(unset).collect();
                keys.extend(known.into_iter().map(str::to_owned));
                keys.sort();
                keys.dedup();
                answer.listed(group::CONFIG_KEYS, &keys);
            }
            "path-add" => self.paths_to_add(asked, answer)?,
            "path-tracked" => self.tracked_paths(asked, answer)?,
            _ => {}
        }
        Ok(())
    }

    /// The names of the refs under `prefix`, without it, in order.
    fn refs_under(&self, prefix: &str) -> Vec<String> {
        let names = self.store.refs.iter();
        let mut under: Vec<String> = names
            .filter_map(|(name, _)| name.strip_prefix(prefix))
            .map(str::to_owned)
            .collect();
        under.sort();
        under
    }

    /// `<remote>/<branch>` for each branch fetched from a remote.
    fn remote_branches(&self) -> Vec<String> {
        let mut names = self.refs_under("refs/remotes/");
        names.retain(|name| !name.ends_with("/HEAD"));
        names
    }

    /// What names a commit: the branches, tags and remote branches,
    /// `HEAD`, and the commits whose id begins with what is typed.
    fn revisions(&self, typed: &str, answer: &mut Answer) -> Res<()> {
        answer.listed(group::BRANCHES, &self.refs_under("refs/heads/"));
        answer.listed(group::TAGS, &self.refs_under("refs/tags/"));
        answer.listed(group::REMOTE_BRANCHES, &self.remote_branches());
        answer.group(group::COMMITS);
        answer.candidate("HEAD", "the commit checked out");
        if typed.len() < 4 || !typed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(());
        }
        let matching = self
            .store
            .odb
            .lookup_by_prefix(&typed.to_ascii_lowercase(), 1000)?;
        let commits = matching
            .iter()
            .filter(|entry| entry.kind == ObjectKind::Commit);
        for entry in commits.take(COMMITS) {
            let Some(object) = self.store.odb.get(&entry.git)? else {
                continue;
            };
            let commit = alt_git_codec::Commit::parse(&object.data)?;
            let subject = commit.message().lines().next().unwrap_or_default();
            answer.candidate(&entry.git.to_string(), &String::from_utf8_lossy(subject));
        }
        Ok(())
    }

    /// What `push` takes: a branch or a notes ref, and behind a `:` the
    /// branches the named remote has.
    fn refspecs(&self, asked: &Asked, answer: &mut Answer) -> Res<()> {
        let forced = if asked.typed.starts_with('+') {
            "+"
        } else {
            ""
        };
        let Some((source, _)) = asked.typed.split_once(':') else {
            if !forced.is_empty() {
                answer.prefix(&format!("{}{forced}", asked.attached));
            }
            answer.listed(group::BRANCHES, &self.refs_under("refs/heads/"));
            let notes = self.store.refs.iter();
            let notes: Vec<&str> = notes
                .map(|(name, _)| name)
                .filter(|name| name.starts_with("refs/notes/"))
                .collect();
            answer.listed(group::REFS, &notes);
            return Ok(());
        };
        answer.prefix(&format!("{}{source}:", asked.attached));
        let remotes = self.read_remotes()?;
        let named = |word: &str| remotes.iter().any(|remote| remote.name == word);
        let mut theirs: Vec<String> = match asked.others().find(|word| named(word)) {
            Some(remote) => self.refs_under(&format!("refs/remotes/{remote}/")),
            None => {
                let all = self.remote_branches();
                let branch =
                    |name: &String| name.split_once('/').map(|(_, branch)| branch.to_owned());
                all.iter().filter_map(branch).collect()
            }
        };
        theirs.retain(|name| name != "HEAD");
        theirs.sort();
        theirs.dedup();
        answer.listed(group::REMOTE_BRANCHES, &theirs);
        Ok(())
    }

    /// The workspaces, each with its directory and the branch it has
    /// checked out.
    fn workspaces(&self, answer: &mut Answer) -> Res<()> {
        answer.group(group::WORKSPACES);
        for (name, path, _) in self.list_workspaces()? {
            let head = match name.as_str() {
                DEFAULT_WORKSPACE => "HEAD".to_owned(),
                _ => format!("workspaces/{name}/HEAD"),
            };
            let branch = match self.store.refs.get(&head) {
                Some(RefTarget::Symbolic(branch)) => {
                    branch.strip_prefix("refs/heads/").unwrap_or(branch)
                }
                _ => "",
            };
            let place = match branch {
                "" => path.display().to_string(),
                _ => format!("{} on {branch}", path.display()),
            };
            answer.candidate(&name, &place);
        }
        Ok(())
    }

    /// What `add` may be given: the paths in conflict first, then those
    /// changed in the working tree and those not tracked.
    fn paths_to_add(&self, asked: &Asked, answer: &mut Answer) -> Res<()> {
        let state = self.tree_state(Untracked::Normal)?;
        let conflicts = state.conflicts();
        let status = state.status();
        let free = |path: &&BString| !conflicts.contains_key(*path);
        let unmerged: Vec<&BString> = conflicts.keys().collect();
        let changed = status.unstaged.iter().map(|(path, _)| path);
        let changed: Vec<&BString> = changed.filter(free).collect();
        let untracked: Vec<&BString> = status.untracked.iter().filter(free).collect();
        self.offer_paths(group::UNMERGED_FILES, &unmerged, asked, answer);
        self.offer_paths(group::MODIFIED_FILES, &changed, asked, answer);
        self.offer_paths(group::UNTRACKED_FILES, &untracked, asked, answer);
        Ok(())
    }

    fn tracked_paths(&self, asked: &Asked, answer: &mut Answer) -> Res<()> {
        let snapshot = self.worktree_snapshot()?;
        let tracked: Vec<BString> = (0..snapshot.entries.len())
            .map(|at| BString::from(snapshot.path(at)))
            .collect();
        let mut tracked: Vec<&BString> = tracked.iter().collect();
        tracked.dedup();
        self.offer_paths(group::TRACKED_FILES, &tracked, asked, answer);
        Ok(())
    }

    /// One group of paths, named as from where the command was run. The
    /// paths already on the line are left out.
    fn offer_paths(
        &self,
        group: group::Group,
        paths: &[&BString],
        asked: &Asked,
        answer: &mut Answer,
    ) {
        let shown = paths
            .iter()
            .filter_map(|path| String::from_utf8(pathspec::shown_from(&self.prefix, path)).ok());
        let given: Vec<&str> = asked.others().collect();
        let shown: Vec<String> = shown
            .filter(|path| !given.contains(&path.as_str()))
            .collect();
        let (files, directories) = paths::offered(shown, asked.typed);
        answer.listed(group, &files);
        if !directories.is_empty() {
            answer.listed(group, &directories);
            answer.nospace();
        }
    }
}
