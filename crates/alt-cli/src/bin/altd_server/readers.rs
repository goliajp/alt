//! The repository a request reads, kept open from one request to the
//! next.
//!
//! A repository opened anew for every request opens its object store
//! under `odb.lock`, shared, and a push holds that lock from its first
//! object to its ref transaction: a fetch then waits for the whole pack
//! of a push to be written. A handle that was open before reads on while
//! the store is appended to, and is brought up to date at the start of
//! each request ([`Repository::follow_behind_writers`]): by the refs
//! first, and by the object store unless the refs are as they were and a
//! writer is in the middle of its objects, none of which a ref names yet.

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;

use alt_cli::held::Held;
use alt_repo::Repository;

use super::WriteCoordinator;

/// The handles of one repository that no request reads through now.
pub(super) type Readers = Arc<Held<Vec<Repository>>>;

pub(super) fn none() -> Readers {
    Arc::new(Held::new(Vec::new()))
}

/// A resolved repo binding for one request. The `WriteCoordinator` is
/// shared across requests for write serialisation + group fsync.
#[derive(Clone)]
pub(super) struct RepoHandle {
    repo_path: PathBuf,
    pub(super) writer: Option<Arc<WriteCoordinator>>,
    readers: Readers,
}

/// A repository in the hand of one request, given back when it ends.
pub(super) struct Reading {
    repo: Option<Repository>,
    readers: Readers,
}

impl RepoHandle {
    pub(super) fn new(
        repo_path: PathBuf,
        writer: Option<Arc<WriteCoordinator>>,
        readers: &Readers,
    ) -> Self {
        RepoHandle {
            repo_path,
            writer,
            readers: Arc::clone(readers),
        }
    }

    /// The repository as it is now: a push that was answered is seen by
    /// the request that comes after it.
    pub(super) fn open_repo(&self) -> Result<Reading, Box<dyn std::error::Error>> {
        let kept = self.readers.take().pop();
        let repo = match kept {
            Some(mut repo) => {
                repo.follow_behind_writers()?;
                repo
            }
            None => Repository::discover(&self.repo_path)?,
        };
        Ok(Reading {
            repo: Some(repo),
            readers: Arc::clone(&self.readers),
        })
    }
}

impl Deref for Reading {
    type Target = Repository;

    fn deref(&self) -> &Repository {
        self.repo.as_ref().expect("until it is dropped")
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        // what a panic left of it is not read through again
        if !std::thread::panicking() {
            self.readers.take().extend(self.repo.take());
        }
    }
}
