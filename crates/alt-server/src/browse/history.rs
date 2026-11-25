//! First-parent history, a page at a time.
use super::{READ, Reader, STRUCTURED, WORK, display};
use alt_git_codec::{Commit, ObjectId};
use alt_server_store::{Credential, Error, Page, Result, Store};

#[derive(alt_json::ToJson)]
pub struct CommitSummary {
    pub(super) oid: String,
    pub(super) parents: Vec<String>,
    pub(super) author: String,
    pub(super) timestamp: String,
    pub(super) message: String,
}
/// First-parent history. The projection includes all parents so the caller can
/// explicitly inspect other ancestry; it never hides this traversal choice.
pub fn history(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    after: Option<ObjectId>,
    limit: usize,
) -> Result<Page<CommitSummary>> {
    if limit == 0 || limit > 100 {
        return Err(Error::Limit("history page size"));
    }
    let info = store.repository_info(credential, repository)?;
    let start = match after {
        Some(oid) => {
            super::reached::led_to(store, credential, repository, oid)?;
            Some(oid)
        }
        None => info
            .refs
            .get(&info.default_branch)
            .map(|id| id.parse().map_err(|_| Error::Integrity("default ref")))
            .transpose()?,
    };
    let Some(start) = start else {
        return Ok(Page {
            items: vec![],
            next: None,
        });
    };
    let mut reader = Reader {
        store,
        credential,
        repository,
        remaining: WORK,
    };
    page(start, limit, STRUCTURED, |oid| reader.commit(oid))
}

/// The page that begins at `start`, each commit as `read` gives it. A message is
/// written whole, and written it can be several times what the commit is: the
/// page ends before the commit that would take what is written of it past
/// `bound`, and that commit begins the next. So does a commit there is nothing
/// left to read with of what a page may read. The first commit of a page is
/// written whatever its size, so every page goes on.
fn page(
    start: ObjectId,
    limit: usize,
    bound: usize,
    mut read: impl FnMut(ObjectId) -> Result<(ObjectId, Commit)>,
) -> Result<Page<CommitSummary>> {
    let mut pending = Some(start);
    let mut items = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut written = 0;
    while items.len() < limit {
        let Some(asked) = pending else {
            break;
        };
        if !seen.insert(asked) {
            return Err(Error::Integrity("commit cycle"));
        }
        let (oid, commit) = match read(asked) {
            // what a page may read is used up: this commit begins the next
            Err(Error::Limit(READ)) if !items.is_empty() => break,
            other => other?,
        };
        let parents = commit.parents().collect::<Vec<_>>();
        let summary = CommitSummary {
            oid: oid.to_string(),
            parents: parents.iter().map(ToString::to_string).collect(),
            author: commit.author().map(|v| display(v)).unwrap_or_default(),
            timestamp: commit
                .committer_date()
                .map(|v| v.to_string())
                .unwrap_or_default(),
            message: String::from_utf8_lossy(commit.message()).into_owned(),
        };
        written += alt_json::to_vec(&summary)?.len();
        if written > bound && !items.is_empty() {
            break;
        }
        pending = parents.first().copied();
        items.push(summary);
    }
    Ok(Page {
        items,
        next: pending.map(|v| v.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::{HashAlgo, ObjectKind};

    /// a line of `count` commits, each with a message of `bytes` bytes that are
    /// written as six each
    fn line(count: usize, bytes: usize) -> Vec<(ObjectId, Vec<u8>)> {
        let mut commits: Vec<(ObjectId, Vec<u8>)> = Vec::new();
        for _ in 0..count {
            let parent = commits.last().map(|(oid, _)| format!("parent {oid}\n"));
            let mut raw = format!(
                "tree {}\n{}author A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\n",
                "0".repeat(40),
                parent.unwrap_or_default()
            )
            .into_bytes();
            raw.resize(raw.len() + bytes, 1);
            let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &raw);
            commits.push((oid, raw));
        }
        commits
    }

    #[test]
    fn a_page_of_history_ends_before_the_commit_that_takes_it_past_what_a_page_is_written_in() {
        let commits = line(5, 1000);
        let tip = commits.last().unwrap().0;
        let read = |oid: ObjectId| {
            let found = commits.iter().find(|(id, _)| *id == oid).unwrap();
            Ok((oid, Commit::parse(&found.1).unwrap()))
        };
        let written = |page: &Page<CommitSummary>| alt_json::to_vec(&page.items).unwrap().len();
        // each commit is written in a little over six thousand bytes
        let all = page(tip, 100, usize::MAX, read).unwrap();
        assert_eq!((all.items.len(), all.next.is_none()), (5, true));
        assert!(written(&all) > 30_000);
        let some = page(tip, 100, 13_000, read).unwrap();
        assert_eq!(some.items.len(), 2);
        assert!(written(&some) <= 13_000);
        // the next page begins with the commit that did not fit
        assert_eq!(some.next, Some(commits[2].0.to_string()));
        let rest = page(commits[2].0, 100, 13_000, read).unwrap();
        assert_eq!(rest.items[0].oid, commits[2].0.to_string());
        // a commit past the bound by itself is a page of its own
        let one = page(tip, 100, 100, read).unwrap();
        assert_eq!(one.items.len(), 1);
        assert_eq!(one.next, Some(commits[3].0.to_string()));

        // what a page may read runs out at its third commit: it begins the next
        let mut reads = 0;
        let within = |oid: ObjectId| {
            reads += 1;
            if reads == 3 {
                return Err(Error::Limit(READ));
            }
            read(oid)
        };
        let short = page(tip, 100, usize::MAX, within).unwrap();
        assert_eq!(short.items.len(), 2);
        assert_eq!(short.next, Some(commits[2].0.to_string()));
        // a first commit that cannot be read is a page that fails
        let none = page(tip, 100, usize::MAX, |_| Err(Error::Limit(READ)));
        assert!(matches!(none, Err(Error::Limit(_))));
    }
}
