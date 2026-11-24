//! What a merge brings in: the commit a word of the command line names,
//! and what git calls it in the message of the merge commit.

use super::*;

/// The side a merge brings in.
pub(in crate::native) struct Brought {
    pub commit: ObjectId,
    /// what the markers of a conflict call the side
    pub label: String,
    /// how the message begins: `Merge branch 'topic'`
    pub title: String,
    /// the message of the annotated tag that was named, which the
    /// message of the merge commit carries
    pub note: Option<String>,
}

impl Brought {
    /// A branch by its short name at `commit`.
    pub(in crate::native) fn branch(name: &str, commit: ObjectId) -> Self {
        Brought {
            commit,
            label: name.to_owned(),
            title: format!("Merge branch '{name}'"),
            note: None,
        }
    }

    /// The message of the commit that merges this into the branch `into`,
    /// which is named unless `unnamed`.
    fn message(&self, into: &str, unnamed: bool) -> String {
        let mut message = match unnamed {
            true => format!("{}\n", self.title),
            false => format!("{} into {into}\n", self.title),
        };
        if let Some(note) = &self.note {
            message.push('\n');
            message.push_str(note);
            if !note.ends_with('\n') {
                message.push('\n');
            }
        }
        message
    }
}

/// `word` without the `^`s or the one `~<n>` it ends with, where that
/// leaves an earlier commit of what the rest names; whether it does is
/// told with it. `name~0` is the name itself.
fn early_part(word: &str) -> Option<(&str, bool)> {
    let rest = word.trim_end_matches('^');
    if rest.len() < word.len() {
        return Some((rest, true));
    }
    let (rest, back) = word.rsplit_once('~')?;
    if !back.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((rest, back.is_empty() || back.bytes().any(|b| b != b'0')))
}

impl NativeRepo<'_> {
    /// What `word` brings to a merge, as git reads it: any revision that
    /// names a commit, `-` being the branch left last.
    pub(in crate::native) fn brought(&self, word: &str) -> Res<Brought> {
        let word = if word == "-" { "@{-1}" } else { word };
        let refuse = |why: String| {
            let said = format!("{why}merge: {word} - not something we can merge\n");
            said::refused(&said, 1)
        };
        let spec = pathspec::at_head(&self.head_ref, word);
        let repo = self.store.git_view()?;
        let Some(named) = repo.rev_parse(&spec)? else {
            return Err(refuse(String::new()));
        };
        let (commit, object) = repo.peel(named)?;
        if object.kind != ObjectKind::Commit {
            let kind = object.kind.as_str();
            return Err(refuse(format!(
                "error: {word}: expected commit type, but the object dereferences to {kind} type\n"
            )));
        }
        let (label, mut note) = (word.to_owned(), None);
        let full = repo.full_name(&spec)?;
        let title = match full.as_deref() {
            Some(left) if word.starts_with("@{-") => {
                let short = left.strip_prefix("refs/heads/").unwrap_or(left);
                format!("Merge branch '{short}'")
            }
            Some(full) if full.starts_with("refs/heads/") => format!("Merge branch '{word}'"),
            Some(full) if full.starts_with("refs/tags/") => {
                if named != commit {
                    let tag = repo.read_object(&named)?.ok_or("tag missing from store")?;
                    let tag = alt_git_codec::Tag::parse(&tag.data)?;
                    note = Some(unsigned(&String::from_utf8_lossy(tag.message())).to_owned());
                }
                format!("Merge tag '{word}'")
            }
            Some(full) if full.starts_with("refs/remotes/") => {
                format!("Merge remote-tracking branch '{word}'")
            }
            _ => match early_part(word) {
                Some((branch, early))
                    if repo.resolve_ref(&format!("refs/heads/{branch}"))?.is_some() =>
                {
                    let early = if early { " (early part)" } else { "" };
                    format!("Merge branch '{branch}'{early}")
                }
                _ => format!("Merge commit '{word}'"),
            },
        };
        Ok(Brought {
            commit,
            label,
            title,
            note,
        })
    }
}

impl NativeRepo<'_> {
    /// How a merge treats a branch that is behind what it brings where
    /// the command line does not say: `merge.ff`, as git reads it.
    pub fn configured_ff(&self) -> Res<FastForward> {
        let settings = self.settings()?;
        if settings
            .get_str("merge", None, "ff")
            .is_some_and(|v| v.as_bytes() == b"only")
        {
            return Ok(FastForward::Only);
        }
        // a value that is neither is as none
        Ok(match settings.get_bool("merge", None, "ff") {
            Some(Ok(false)) => FastForward::Never,
            _ => FastForward::Allowed,
        })
    }

    /// The message of the commit that merges `brought` into the branch
    /// `into`, whose commit is `ours`: the branch is named unless a
    /// pattern of `merge.suppressDest` matches it, `main` and `master`
    /// where none is set, and with `merge.log` the commits brought are
    /// listed after it, as git lists them.
    pub(in crate::native) fn merge_message(
        &self,
        brought: &Brought,
        ours: ObjectId,
        into: &str,
    ) -> Res<String> {
        let settings = self.settings()?;
        let mut patterns: Vec<&[u8]> = vec![b"main", b"master"];
        let set = settings.get_all("merge", None, "suppressdest");
        for (at, pattern) in set.iter().enumerate() {
            // the first that is set takes the place of what git has without one
            if at == 0 {
                patterns.clear();
            }
            match pattern.filter(|pattern| !pattern.is_empty()) {
                Some(pattern) => patterns.push(pattern),
                None => patterns.clear(),
            }
        }
        let matches = |p: &&[u8]| alt_git_config::wildmatch(p, into.as_bytes(), false);
        let mut message = brought.message(into, patterns.iter().any(matches));
        // a number is how many are listed, and a word says whether
        // git's twenty are
        let limit = match settings.get_int("merge", None, "log") {
            Some(Ok(limit)) => usize::try_from(limit).unwrap_or(0),
            _ => match settings.get_bool("merge", None, "log") {
                Some(Ok(true)) => 20,
                _ => 0,
            },
        };
        if limit > 0 {
            message.push_str(&self.brought_listed(brought, ours, limit)?);
        }
        Ok(message)
    }

    /// The commits `brought` has and `ours` lacks, the newest first and
    /// no merge among them, each by its subject: at most `limit`, with
    /// how many there are where there are more.
    fn brought_listed(&self, brought: &Brought, ours: ObjectId, limit: usize) -> Res<String> {
        let repo = self.store.git_view()?;
        let mut had = std::collections::HashSet::new();
        for walked in repo.rev_walk(ours)? {
            had.insert(walked?.0);
        }
        let (mut count, mut subjects) = (0, Vec::new());
        for walked in repo.rev_walk(brought.commit)? {
            let commit = walked?.0;
            if had.contains(&commit) {
                continue;
            }
            let parsed = repo.read_commit(&commit)?;
            if parsed.parents().count() > 1 {
                continue;
            }
            count += 1;
            if subjects.len() > limit {
                continue;
            }
            subjects.push(subject(parsed.message()).unwrap_or_else(|| commit.to_string()));
        }
        // a branch is listed by its name, anything else as the title has it
        let title = brought
            .title
            .strip_prefix("Merge ")
            .unwrap_or(&brought.title);
        let name = match title.starts_with("tag ") || title.starts_with("commit ") {
            true => title,
            false => title.split('\'').nth(1).unwrap_or(title),
        };
        let mut listed = match count > limit {
            true => format!("\n* {name}: ({count} commits)\n"),
            false => format!("\n* {name}:\n"),
        };
        for (at, subject) in subjects.iter().enumerate() {
            listed.push_str(&match at >= limit {
                true => "  ...\n".to_owned(),
                false => format!("  {subject}\n"),
            });
        }
        Ok(listed)
    }
}

/// The first paragraph of `message` on one line, as git's `%s` has it;
/// `None` where it is empty.
fn subject(message: &[u8]) -> Option<String> {
    let message = String::from_utf8_lossy(message);
    let first = message.trim_start_matches('\n').split("\n\n").next();
    let lines: Vec<&str> = first.unwrap_or_default().lines().collect();
    let subject = lines.join(" ");
    Some(subject.trim_start().to_owned()).filter(|subject| !subject.is_empty())
}

/// The message of a tag without the signature it ends with.
fn unsigned(message: &str) -> &str {
    let signed = [
        "-----BEGIN PGP SIGNATURE-----",
        "-----BEGIN SSH SIGNATURE-----",
    ];
    let cut = signed.iter().filter_map(|begin| message.find(begin)).min();
    &message[..cut.unwrap_or(message.len())]
}

#[cfg(test)]
mod tests {
    use super::early_part;

    #[test]
    fn an_earlier_commit_of_a_name_is_told_from_the_name() {
        assert_eq!(early_part("side~1"), Some(("side", true)));
        assert_eq!(early_part("side~"), Some(("side", true)));
        assert_eq!(early_part("side~0"), Some(("side", false)));
        assert_eq!(early_part("side^^"), Some(("side", true)));
        assert_eq!(early_part("side~1^"), Some(("side~1", true)));
        assert_eq!(early_part("side~x"), None);
        assert_eq!(early_part("side"), None);
    }
}
