//! What git would have logged of a move a native store recorded, as far
//! as the operation tells: where git writes the word typed for a commit,
//! the id of the commit stands. A merge is told with the branch merged
//! where the message of its commit names it, or where one branch alone
//! was at the commit the merge moved on to.

use alt_bytes::ByteSlice;
use alt_git_codec::ObjectId;

use super::{Move, Said};
use crate::{RepoError, Repository};

impl Repository {
    fn subject(&self, commit: ObjectId) -> Result<Vec<u8>, RepoError> {
        let parsed = self.read_commit(&commit)?;
        let subject = parsed.message().split(|b| *b == b'\n').next();
        Ok(subject.unwrap_or_default().to_vec())
    }

    /// Whether `old` is among the ancestors of `new`.
    fn reaches(&self, new: ObjectId, old: ObjectId) -> Result<bool, RepoError> {
        for walked in self.rev_walk(new)? {
            if walked?.0 == old {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The line git logs of a merge that took `old` to `new`.
    fn merged(
        &self,
        old: ObjectId,
        new: ObjectId,
        tip: &Option<String>,
    ) -> Result<Vec<u8>, RepoError> {
        let parents: Vec<ObjectId> = self.read_commit(&new)?.parents().collect();
        let (of, how) = match parents[..] {
            [first, second, ..] if first == old => {
                let subject = self.subject(new)?;
                let named = subject
                    .strip_prefix(b"Merge branch '")
                    .and_then(|rest| rest.find_byte(b'\'').map(|end| rest[..end].to_vec()));
                let of = named.unwrap_or_else(|| second.to_string().into_bytes());
                (of, "Merge made by the 'ort' strategy.")
            }
            _ => {
                let of = tip.clone().unwrap_or_else(|| new.to_string());
                (of.into_bytes(), "Fast-forward")
            }
        };
        Ok([b"merge ", &of[..], b": ", how.as_bytes()].concat())
    }

    pub(super) fn tell(&self, name: &str, moved: &Move) -> Result<Vec<u8>, RepoError> {
        let Said::Of {
            verb,
            from,
            to,
            onto,
            tip,
        } = &moved.said
        else {
            return Ok(Vec::new());
        };
        let Some(new) = moved.new else {
            return Ok(Vec::new());
        };
        let branch = |name: &Option<String>| {
            let short = name.as_deref()?.strip_prefix("refs/heads/")?;
            Some(short.to_owned())
        };
        let with_subject = |what: &str| -> Result<Vec<u8>, RepoError> {
            Ok([what.as_bytes(), b": ", &self.subject(new)?[..]].concat())
        };
        let said = match (verb.as_str(), branch(from), branch(to)) {
            (_, Some(from), Some(to)) => format!("checkout: moving from {from} to {to}"),
            ("rebase", Some(_), None) => format!("rebase (start): checkout {new}"),
            ("rebase", None, Some(_)) => {
                let to = to.as_deref().unwrap_or_default();
                format!("rebase (finish): returning to {to}")
            }
            ("rebase", ..) => match onto {
                Some(onto) => format!("rebase (finish): {name} onto {onto}"),
                None => return with_subject("rebase (pick)"),
            },
            ("commit", ..) => {
                return with_subject(match self.read_commit(&new)?.parents().count() {
                    0 => "commit (initial)",
                    1 => "commit",
                    _ => "commit (merge)",
                });
            }
            ("amend", ..) => return with_subject("commit (amend)"),
            ("cherry-pick", ..) => return with_subject("cherry-pick"),
            ("revert", ..) => return with_subject("revert"),
            ("reset", ..) => format!("reset: moving to {new}"),
            ("merge" | "flow", ..) if moved.old.is_some() => {
                return self.merged(moved.old.expect("a move from a commit"), new, tip);
            }
            ("fetch", ..) if name.starts_with("refs/remotes/") => {
                let rest = &name["refs/remotes/".len()..];
                let remote = rest.split('/').next().unwrap_or(rest);
                let how = match moved.old {
                    None => "storing head",
                    Some(old) if self.reaches(new, old)? => "fast-forward",
                    Some(_) => "forced-update",
                };
                format!("fetch {remote}: {how}")
            }
            _ if moved.old.is_none() && name.starts_with("refs/heads/") => {
                format!("branch: Created from {new}")
            }
            _ => format!("{verb}: moving to {new}"),
        };
        Ok(said.into_bytes())
    }
}
