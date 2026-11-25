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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alt_cli::held::Held;
use alt_cli::native::owned;
use alt_git_codec::ObjectId;
use alt_repo::Repository;

use super::log::{LogCtx, respond_logged};
use super::{WriteCoordinator, http};

/// The handles of one repository that no request reads through now.
pub(super) type Readers = Arc<Held<Vec<Repository>>>;

/// Where the repository `name` of a URL lies under `root`. The name is
/// one directory of the root, and that directory is the repository: one
/// that is none is not looked for in the directories above it, the
/// root's own among them.
pub(super) fn named(root: &Path, name: &str) -> Result<PathBuf, String> {
    let odd = |c: char| c == '\\' || c.is_control();
    if name.is_empty() || name.starts_with('.') || name.contains(odd) {
        return Err("invalid repo name in URL".into());
    }
    let path = root.join(name);
    if !path.join(".alt").is_dir() && !path.join(".git").exists() {
        return Err(format!("repo '{name}' not found under server root"));
    }
    Ok(path)
}

/// Whether `root` lists a directory as `name`, byte for byte. A file system
/// may open one directory under several writings of its name, in another
/// case or another composition of its letters: a repository asked for under
/// one of those would be held to the rules of another name, and opened a
/// second time beside itself.
pub(super) fn listed(root: &Path, name: &str) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(root)? {
        if entry?.file_name() == name {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What tells the directory of a repository from another under its name,
/// and that directory open: while it is, its number is given to no other,
/// and one put in its place is not taken for it. Only a repository that
/// has no `.alt` is told by its `.git`: one whose `.alt` could not be
/// looked at this once is not taken for another, to be opened a second
/// time beside itself.
pub(super) fn directory(repo_path: &Path) -> std::io::Result<((u64, u64), std::fs::File)> {
    use std::os::unix::fs::MetadataExt;
    let open = match std::fs::File::open(repo_path.join(".alt")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::File::open(repo_path.join(".git"))?
        }
        open => open?,
    };
    let about = open.metadata()?;
    Ok(((about.dev(), about.ino()), open))
}

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

    /// Whether no request has this repository in hand, and nothing is
    /// lost with what is kept of it: where a sync of it failed, that it
    /// did is known by its writer alone.
    pub(super) fn idle(&self) -> bool {
        let written = |writer: &Arc<WriteCoordinator>| {
            Arc::strong_count(writer) == 1 && writer.group.failed().is_none()
        };
        Arc::strong_count(&self.readers) == 1 && self.writer.as_ref().is_none_or(written)
    }

    /// The repository as it is now: a push that was answered is seen by
    /// the request that comes after it.
    pub(super) fn open_repo(&self) -> Result<Reading, Box<dyn std::error::Error>> {
        let kept = self.readers.take().pop();
        let repo = match kept {
            Some(mut repo) => {
                owned::ensure_served(&repo)?;
                repo.follow_behind_writers()?;
                repo
            }
            None => {
                // the object store is opened now, whatever the request
                // is for: a handle that has read it reads on behind a
                // writer, and one that has not waits for the writer when
                // it comes to its first object
                let repo = owned::discover_served(&self.repo_path)?;
                let algo = repo.algo()?;
                let none = ObjectId::from_bytes(algo, &vec![0; algo.raw_len()])?;
                repo.read_object(&none)?;
                repo
            }
        };
        Ok(Reading {
            repo: Some(repo),
            readers: Arc::clone(&self.readers),
        })
    }
}

impl Reading {
    /// Answers the request the repository was read for, the repository
    /// given back first. The peer sends its next request when it has the
    /// answer, and that request is to find this handle: one opened anew
    /// for it waits for a push that is writing its objects. A handler takes
    /// its `Reading` as its last argument, which is dropped before the
    /// request is, for the same to hold of a request it lets go of
    /// unanswered.
    pub(super) fn answer<R: std::io::Read + Send + 'static>(
        self,
        req: http::Request,
        resp: http::Response<R>,
        log: &mut LogCtx,
    ) -> std::io::Result<()> {
        drop(self);
        respond_logged(req, resp, log)
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
