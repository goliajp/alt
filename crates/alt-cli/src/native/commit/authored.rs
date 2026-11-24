//! Whom a commit is by where the command line says: `--author`, `--date`
//! and `--reset-author`.

use super::*;

/// What `alt commit` was told of the author.
#[derive(Debug, Default, Clone)]
pub struct Authored {
    /// `--author`: `Name <email>`, or a text an earlier author holds.
    pub author: Option<String>,
    /// `--date`
    pub date: Option<String>,
    /// `--reset-author`
    pub reset: bool,
}

/// The author of a commit about to be written.
pub(in crate::native) struct Author {
    name: String,
    email: String,
    when: i64,
    tz: String,
}

impl Author {
    pub(in crate::native) fn sig(&self) -> Sig<'_> {
        Sig {
            name: &self.name,
            email: &self.email,
            when: self.when,
            tz: &self.tz,
        }
    }
}

impl NativeRepo<'_> {
    /// The author of a commit: `kept` is whom it would be by with no
    /// word of it on the command line, the caller where it is `None`.
    pub(in crate::native) fn authored(
        &self,
        said: &Authored,
        kept: Option<&Sig<'_>>,
    ) -> Res<Author> {
        if said.reset && said.author.is_some() {
            let both = "options '--reset-author' and '--author' cannot be used together";
            return Err(settings::fatal(both.to_owned()));
        }
        let named = said.author.as_deref().map(|text| self.author_named(text));
        let named = named.transpose()?;
        let mut author = match kept.filter(|_| !said.reset) {
            Some(kept) => Author {
                name: kept.name.to_owned(),
                email: kept.email.to_owned(),
                when: kept.when,
                tz: kept.tz.to_owned(),
            },
            // the caller need not be known where the author is named
            None if named.is_some() => {
                let (when, tz) = self.author_moment()?;
                let (name, email) = Default::default();
                Author {
                    name,
                    email,
                    when,
                    tz,
                }
            }
            None => {
                let me = self.author()?;
                Author {
                    name: me.name().to_owned(),
                    email: me.email().to_owned(),
                    when: me.when(),
                    tz: me.tz().to_owned(),
                }
            }
        };
        if let Some((name, email)) = named {
            (author.name, author.email) = (name, email);
        }
        if let Some(date) = &said.date {
            (author.when, author.tz) = self.date_said(date)?;
        }
        Ok(author)
    }

    /// The name and the address `--author` gives: written out as
    /// `Name <email>`, or those of the latest commit HEAD has whose
    /// author holds the text.
    fn author_named(&self, text: &str) -> Res<(String, String)> {
        let trimmed = |part: &str| part.trim().to_owned();
        if let Some((name, rest)) = text.split_once('<')
            && let Some((email, _)) = rest.split_once('>')
        {
            if name.trim().is_empty() {
                let empty = format!("empty ident name (for <{}>) not allowed", email.trim());
                return Err(settings::fatal(empty));
            }
            return Ok((trimmed(name), trimmed(email)));
        }
        let wanted = text.to_lowercase();
        let mut latest: Option<(i64, String, String)> = None;
        let head = self.store.refs.resolve(&self.head_branch()?)?;
        for commit in self.ancestry(&head.into_iter().collect::<Vec<_>>())?.keys() {
            let stored = self.store.odb.get(commit)?;
            let stored = stored.ok_or("commit missing from store")?;
            let parsed = alt_git_codec::Commit::parse(&stored.data)?;
            let Some(Ok(line)) = parsed.author().map(|line| line.to_str()) else {
                continue;
            };
            let by = parse_sig(line)?;
            let who = format!("{} <{}>", by.name, by.email);
            let made = parsed.committer_date().unwrap_or(0);
            let later = latest.as_ref().is_none_or(|(at, ..)| made > *at);
            if later && who.to_lowercase().contains(&wanted) {
                latest = Some((made, by.name.to_owned(), by.email.to_owned()));
            }
        }
        match latest {
            Some((_, name, email)) => Ok((name, email)),
            None => Err(settings::fatal(format!(
                "--author '{text}' is not 'Name <email>' and matches no existing author"
            ))),
        }
    }
}
