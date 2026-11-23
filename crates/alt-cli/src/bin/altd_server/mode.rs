//! What is served: one repository or a directory of them, and what a
//! repository that is written to has besides its store.

use super::*;

/// Single-repo (ALT_SERVER_REPO) or multi-repo (ALT_SERVER_ROOT) serve.
/// Single keeps the original shape — info/refs lives at the URL root.
/// Multi parses the first path segment as a repo name and resolves it
/// under the configured root.
pub(super) enum ServeMode {
    Single {
        repo_path: PathBuf,
        writer: Option<Arc<WriteCoordinator>>,
        readers: readers::Readers,
    },
    Multi {
        root: PathBuf,
        cache: Held<Opened>,
    },
}

/// The repositories of a root that are open: each keeps its store, the
/// handles it is read through and their files open, so no more are kept
/// than `most`. One that is come to past that has the one used longest
/// ago, of those no request has in hand, closed for it.
pub(super) struct Opened {
    repos: HashMap<String, Open>,
    clock: u64,
    most: usize,
}

struct Open {
    handle: RepoHandle,
    used: u64,
    /// the directory it was opened in: another one under the same name
    /// is another repository
    dir: (u64, u64),
}

impl Opened {
    /// The repository open under `name`, when it is still the one in
    /// `dir`; one whose directory was replaced is closed.
    fn known(&mut self, name: &str, dir: (u64, u64)) -> Option<RepoHandle> {
        self.clock += 1;
        match self.repos.get_mut(name) {
            Some(open) if open.dir == dir => {
                open.used = self.clock;
                Some(open.handle.clone())
            }
            Some(_) => {
                self.repos.remove(name);
                None
            }
            None => None,
        }
    }

    /// Keeps `opened` under `name` unless another request did so first,
    /// and closes what is over the number that may be open.
    fn keep(&mut self, name: &str, dir: (u64, u64), opened: RepoHandle) -> RepoHandle {
        let used = self.clock;
        let open = Open {
            handle: opened,
            used,
            dir,
        };
        let handle = self
            .repos
            .entry(name.to_owned())
            .or_insert(open)
            .handle
            .clone();
        while self.repos.len() > self.most {
            let idle = |(other, open): &(&String, &Open)| *other != name && open.handle.idle();
            let oldest = self
                .repos
                .iter()
                .filter(idle)
                .min_by_key(|(_, open)| open.used);
            let Some(oldest) = oldest.map(|(name, _)| name.clone()) else {
                break;
            };
            self.repos.remove(&oldest);
        }
        handle
    }
}

/// What tells the directory of a repository from another under its name.
fn directory(repo_path: &std::path::Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let alt = repo_path.join(".alt");
    let about = std::fs::metadata(&alt).or_else(|_| std::fs::metadata(repo_path.join(".git")))?;
    Ok((about.dev(), about.ino()))
}

/// three-piece write coordinator for one repo. `store` is what a push
/// has in hand from its look at the refs to its ref transaction; a
/// request that panics with it in hand ends the process (see
/// [`alt_cli::held`]).
/// `sink` is an independent fsync handle (own fds) that the leader
/// committer calls outside the store lock — overlap is what makes
/// group commit pay off. `group` hands out tickets + makes followers
/// wait for the leader's flush, so N concurrent receive-pack pushes
/// share ~1 fsync instead of paying N fsyncs each. Shared across
/// requests via `Arc`.
pub(super) struct WriteCoordinator {
    pub(super) store: Held<Store>,
    pub(super) sink: alt_cli::native::StoreSink,
    pub(super) group: alt_cli::group_commit::GroupCommit,
    /// single-use nonces issued during receive-pack info/refs
    /// and consumed during the matching `git-receive-pack` POST. Bounds
    /// the replay window to "between info/refs advert and EOL of the
    /// LRU table". A captured signed push payload can only be replayed
    /// against this server if its nonce hasn't yet been consumed and
    /// hasn't aged out of the table.
    pub(super) nonces: NonceTable,
}

/// Open the write store and build the write coordinator: turn on deferred
/// durability so each commit appends-without-fsync, create the off-write
/// `sink` so the fsync uses independent fds, instantiate the
/// `GroupCommit` that coalesces concurrent flushes.
pub(super) fn open_write_coordinator(
    alt_dir: std::path::PathBuf,
) -> Result<Arc<WriteCoordinator>, Box<dyn std::error::Error>> {
    let mut store = Store::open(alt_dir)?;
    store.set_defer_durability(true);
    let sink = store.sink()?;
    Ok(Arc::new(WriteCoordinator {
        store: Held::new(store),
        sink,
        group: alt_cli::group_commit::GroupCommit::new(),
        nonces: NonceTable::new(),
    }))
}

impl ServeMode {
    pub(super) fn single(repo_path: PathBuf) -> Self {
        // Fail-fast: probe the repo once at boot so a misconfigured
        // ALT_SERVER_REPO surfaces immediately rather than on the first
        // request. We drop the handle; per-request opens are cheap.
        Repository::discover(&repo_path).unwrap_or_else(|e| die(&format!("opening repo: {e}")));
        let alt_dir = repo_path.join(".alt");
        let writer: Option<Arc<WriteCoordinator>> = if alt_dir.is_dir() {
            Some(
                open_write_coordinator(alt_dir)
                    .unwrap_or_else(|e| die(&format!("opening write coordinator: {e}"))),
            )
        } else {
            eprintln!(
                "altd-server: no .alt under {p}; receive-pack will be refused (read-only mode)",
                p = repo_path.display()
            );
            None
        };
        let readers = readers::none();
        ServeMode::Single {
            repo_path,
            writer,
            readers,
        }
    }

    pub(super) fn multi(root: PathBuf) -> Self {
        if !root.is_dir() {
            die(&format!(
                "ALT_SERVER_ROOT={} does not exist or isn't a directory",
                root.display()
            ));
        }
        // fail-open auth guard. When `ALT_SERVER_REQUIRE_AUTH=1`
        // an absent or unreadable `users` file is a hard error at
        // startup — the operator who set the env explicitly opted into
        // "no auth = no serve". Without the env we keep the previous
        // permissive behaviour (no users file = no auth check), which
        // is the right default for `--bind 127.0.0.1` dev loops.
        if std::env::var("ALT_SERVER_REQUIRE_AUTH").as_deref() == Ok("1") {
            let users_path = root.join("users");
            if !users_path.is_file() {
                die(&format!(
                    "ALT_SERVER_REQUIRE_AUTH=1 but {} is missing or unreadable; \
                     refusing to start (fail-open auth disabled)",
                    users_path.display()
                ));
            }
        }
        ServeMode::Multi {
            root,
            cache: Held::new(Opened {
                repos: HashMap::new(),
                clock: 0,
                most: told::told().repos,
            }),
        }
    }

    pub(super) fn describe(&self) -> String {
        match self {
            ServeMode::Single { repo_path, .. } => format!("single repo {}", repo_path.display()),
            ServeMode::Multi { root, .. } => format!("multi-repo under {}", root.display()),
        }
    }
}

/// Resolve the repo the request is targeting + return the remainder of
/// the URL path (so the dispatcher can match on `/info/refs` etc.) plus
/// the repo *name* under multi-repo mode. Single-repo mode has no
/// extractable name; we substitute the synthetic `*` so an ACL rule
/// matching every repo still applies, though scoped ACLs only fire in
/// multi mode anyway.
pub(super) fn resolve_repo(
    mode: &ServeMode,
    path: &str,
) -> Result<(RepoHandle, String, String), Box<dyn std::error::Error>> {
    match mode {
        ServeMode::Single {
            repo_path,
            writer,
            readers,
        } => Ok((
            RepoHandle::new(repo_path.clone(), writer.clone(), readers),
            path.to_owned(),
            "*".to_owned(),
        )),
        ServeMode::Multi { root, cache } => {
            let trimmed = path.trim_start_matches('/');
            let (name, rest) = match trimmed.find('/') {
                Some(i) => (&trimmed[..i], &trimmed[i..]),
                None => (trimmed, ""),
            };
            let repo_path = readers::named(root, name)?;
            let dir = directory(&repo_path)?;
            if let Some(handle) = cache.take().known(name, dir) {
                return Ok((handle, rest.to_owned(), name.to_owned()));
            }
            // opened with the cache let go of: an open that waits for a
            // lock of this repository keeps no request for another waiting
            // Probe once: surfaces "not a repo" errors right at cache
            // insertion rather than on every request.
            Repository::discover(&repo_path)?;
            let alt_dir = repo_path.join(".alt");
            let writer = match alt_dir.is_dir() {
                true => Some(open_write_coordinator(alt_dir)?),
                false => None,
            };
            // the first to have opened it is the one every request uses
            let opened = RepoHandle::new(repo_path, writer, &readers::none());
            let handle = cache.take().keep(name, dir, opened);
            Ok((handle, rest.to_owned(), name.to_owned()))
        }
    }
}
