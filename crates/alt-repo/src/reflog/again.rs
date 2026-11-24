//! The logs of a store that is imported into again. git's logs take the
//! place of those an earlier import kept; a move the store made of a ref
//! since, which git never saw, would go with them. Such a log is kept as
//! the store has it, and the import is its last line: from what the ref
//! named here to what git has it name.

use std::fs;
use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_git_refs::reflog::{Line, parse, write};

use super::{Entry, KEPT};
use crate::{RepoError, Repository};

/// The log of a ref by its full name.
pub type Carried = Vec<(String, Vec<Line>)>;

impl Repository {
    /// Who git would say moved a ref at `entry`: as git logged it where
    /// it did. An operation of the store tells when and no more: the
    /// committer of the commit the ref was moved to stands for who.
    fn moved_by(&self, entry: &Entry) -> Result<Vec<u8>, RepoError> {
        if let Some(by) = &entry.by {
            return Ok(by.clone());
        }
        let seconds = entry.at_ms.unwrap_or_default() / 1000;
        let mut who = Vec::new();
        if let Some(new) = entry.new
            && let Some(committer) = self.read_commit(&new)?.committer()
            && let Some(end) = committer.iter().rposition(|b| *b == b'>')
        {
            who.extend_from_slice(&committer[..=end]);
        }
        who.extend_from_slice(format!(" {seconds} +0000").as_bytes());
        Ok(who)
    }

    /// The log of the ref `name` as the lines git keeps of one.
    pub fn log_lines(&self, name: &str) -> Result<Vec<Line>, RepoError> {
        let mut lines = Vec::new();
        for entry in self.reflog(name)? {
            lines.push(Line {
                old: entry.old,
                new: entry.new,
                by: self.moved_by(&entry)?,
                said: entry.said,
            });
        }
        Ok(lines)
    }
}

/// The logs the store under `alt_dir` has of `names` before an import
/// moves its refs, for [`take`](super::take) to keep what git's lack.
pub fn carried<'a>(
    alt_dir: &Path,
    names: impl Iterator<Item = &'a str>,
) -> Result<Carried, RepoError> {
    let repo = Repository::open_alt(alt_dir.to_owned(), None)?.reading();
    let logged = |name: &&str| {
        *name == "HEAD" || name.starts_with("refs/heads/") || name.starts_with("refs/remotes/")
    };
    let mut found = Carried::new();
    for name in names.filter(logged) {
        let lines = repo.log_lines(name)?;
        if !lines.is_empty() {
            found.push((name.to_owned(), lines));
        }
    }
    Ok(found)
}

/// Writes over the logs just kept under `alt_dir` each log of `ours`
/// that git's does not begin with.
pub(super) fn keep_ours(alt_dir: &Path, ours: Carried, algo: HashAlgo) -> Result<(), RepoError> {
    let kept = alt_dir.join(KEPT);
    for (name, mut lines) in ours {
        let path = kept.join(&name);
        let theirs = super::read(&path)?
            .map(|log| parse(&log))
            .unwrap_or_default();
        let moved = |line: &Line| (line.old, line.new);
        let begins = theirs.len() >= lines.len()
            && lines.iter().zip(&theirs).all(|(a, b)| moved(a) == moved(b));
        if begins {
            continue;
        }
        let here = lines.last().and_then(|line| line.new);
        if let Some(last) = theirs.last().filter(|last| last.new != here)
            && let Some(new) = last.new
        {
            lines.push(Line {
                old: here,
                new: Some(new),
                by: last.by.clone(),
                said: Some(format!("import: moving to {new}").into_bytes()),
            });
        }
        fs::create_dir_all(path.parent().expect("a file under the directory"))?;
        fs::write(path, write(&lines, algo))?;
    }
    Ok(())
}
