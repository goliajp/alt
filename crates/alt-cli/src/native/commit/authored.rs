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
    /// `Name <email>`, or those of the first commit git's `log --all`
    /// comes to whose author the text, read as a pattern, matches. Each
    /// is written as any name or address is: without what cannot stand
    /// in the line of a commit that names its author.
    fn author_named(&self, text: &str) -> Res<(String, String)> {
        let found;
        let written = match text.contains('>') {
            true => text,
            false => {
                found = self.author_matching(text)?;
                &found
            }
        };
        let split = written.split_once('<');
        let split = split.and_then(|(name, rest)| Some((name, rest.split_once('>')?.0)));
        let Some((name, email)) = split else {
            return Err(settings::fatal("malformed --author parameter".to_owned()));
        };
        let email = signature::clean(email).unwrap_or_default();
        match signature::clean(name) {
            Some(name) => Ok((name, email)),
            None => Err(settings::fatal(format!(
                "empty ident name (for <{email}>) not allowed"
            ))),
        }
    }

    /// The author, as `Name <email>`, of the first commit whose author
    /// `text` matches as git's `log --all -i --author=<text>` matches
    /// it: a basic regular expression, with no regard to case. The
    /// commits come newest first by the dates they were committed at,
    /// from every ref and then from HEAD.
    fn author_matching(&self, text: &str) -> Res<String> {
        let options = alt_regex::Options {
            syntax: alt_regex::Syntax::Basic,
            ignore_case: true,
            newline: true,
            ..Default::default()
        };
        let pattern = alt_regex::Regex::with_options(text.as_bytes(), options)
            .map_err(|e| settings::fatal(format!("command line, '{text}': {e}")))?;
        let mut tips = Vec::new();
        let refs = self.store.refs.iter().map(|(name, _)| name.to_owned());
        let refs = refs.filter(|name| name.starts_with("refs/"));
        for name in refs.chain(["HEAD".to_owned()]) {
            tips.extend(self.commit_named(&name)?);
        }
        // newest first; of equal dates the one that came first
        let mut waiting: Vec<(i64, ObjectId)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut wait = |repo: &Self, commit: ObjectId, waiting: &mut Vec<(i64, ObjectId)>| {
            if seen.insert(commit) {
                let at = repo.committer_time(commit)?;
                let behind = waiting.partition_point(|(date, _)| *date >= at);
                waiting.insert(behind, (at, commit));
            }
            Res::Ok(())
        };
        for tip in tips {
            wait(self, tip, &mut waiting)?;
        }
        while !waiting.is_empty() {
            let (_, commit) = waiting.remove(0);
            let stored = self.store.odb.get(&commit)?;
            let stored = stored.ok_or("commit missing from store")?;
            let parsed = alt_git_codec::Commit::parse(&stored.data)?;
            if let Some(line) = parsed.author() {
                // the name and the address, without the date behind them
                let by = line.iter().rposition(|b| *b == b'>');
                let by = &line[..by.map_or(line.len(), |at| at + 1)];
                if pattern.is_match(by) {
                    return Ok(String::from_utf8_lossy(by).into_owned());
                }
            }
            for parent in parsed.parents() {
                wait(self, parent, &mut waiting)?;
            }
        }
        Err(settings::fatal(format!(
            "--author '{text}' is not 'Name <email>' and matches no existing author"
        )))
    }
}
