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
    /// which is named unless it is `main` or `master`.
    pub(in crate::native) fn message(&self, into: &str) -> String {
        let mut message = match into {
            "main" | "master" => format!("{}\n", self.title),
            _ => format!("{} into {into}\n", self.title),
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
