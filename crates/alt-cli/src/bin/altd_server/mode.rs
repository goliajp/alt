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
        /// the one repository, as a root keeps each of its own: another
        /// put in its place is told from it by its directory
        cache: Held<Opened>,
    },
    Multi {
        root: PathBuf,
        cache: Held<Opened>,
        /// Whether the root had a users file when the server started. It
        /// then lets nobody in without one: a file that is gone for a
        /// moment, taken away to be written anew, opens nothing.
        users: bool,
    },
}

/// The repositories of a root that are open: each keeps its store, the
/// handles it is read through and their files open, so no more are kept
/// than `most`. One that is come to past that has the one used longest
/// ago, of those no request has in hand, closed for it.
pub(super) struct Opened {
    repos: HashMap<String, Open>,
    /// the nonces issued for each repository, by its name and directory:
    /// kept while one of them is good, whether the repository is open
    /// or not
    nonces: HashMap<(String, (u64, u64)), Arc<NonceTable>>,
    /// The writer of each repository, by its name and directory, for as
    /// long as a request has it in hand: one that is closed meanwhile,
    /// or put aside for another and put back, is written through the
    /// one writer it has. A writer keeps its directory open, so no other
    /// directory is taken for it.
    writers: HashMap<(String, (u64, u64)), std::sync::Weak<WriteCoordinator>>,
    /// those a sync failed of: kept, so that a repository put aside and
    /// put back is not written to again
    stopped: writer::Stopped,
    clock: u64,
    most: usize,
}

struct Open {
    handle: RepoHandle,
    used: u64,
    /// the directory it was opened in, kept open: another one under the
    /// same name is another repository and has another number
    dir: ((u64, u64), std::fs::File),
}

impl Opened {
    /// The repository open under `name`, when it is still the one in
    /// `dir`; one whose directory was replaced is closed.
    fn known(&mut self, name: &str, dir: (u64, u64)) -> Option<RepoHandle> {
        self.clock += 1;
        match self.repos.get_mut(name) {
            Some(open) if open.dir.0 == dir => {
                open.used = self.clock;
                Some(open.handle.clone())
            }
            Some(_) => {
                let closed = self.repos.remove(name).expect("found there");
                if let Some(writer) = closed.handle.writer {
                    writer.remember();
                }
                None
            }
            None => None,
        }
    }

    /// The nonces of the repository `name` in `dir`.
    fn nonces(&mut self, name: &str, dir: (u64, u64)) -> Arc<NonceTable> {
        let held = |table: &Arc<NonceTable>| Arc::strong_count(table) > 1 || !table.spent();
        self.nonces.retain(|_, table| held(table));
        let table = self.nonces.entry((name.to_owned(), dir)).or_default();
        Arc::clone(table)
    }

    /// The writer there is of the repository `name` in `dir`.
    fn writer(&mut self, name: &str, dir: (u64, u64)) -> Option<Arc<WriteCoordinator>> {
        let key = (name.to_owned(), dir);
        let stopped = self.stopped.lock().expect("no panic with it held");
        let stopped = stopped.get(&key).cloned();
        stopped.or_else(|| self.writers.get(&key)?.upgrade())
    }

    /// Keeps `opened` as that writer unless another request did so first.
    fn written(
        &mut self,
        name: &str,
        dir: (u64, u64),
        opened: Arc<WriteCoordinator>,
    ) -> Arc<WriteCoordinator> {
        self.writers.retain(|_, writer| writer.strong_count() > 0);
        if let Some(first) = self.writer(name, dir) {
            return first;
        }
        let key = (name.to_owned(), dir);
        self.writers.insert(key, Arc::downgrade(&opened));
        opened
    }

    /// Keeps `opened` under `name` unless another request did so first,
    /// and closes what is over the number that may be open.
    fn keep(
        &mut self,
        name: &str,
        dir: ((u64, u64), std::fs::File),
        opened: RepoHandle,
    ) -> RepoHandle {
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

impl Opened {
    fn of(most: usize) -> Held<Opened> {
        Held::new(Opened {
            repos: HashMap::new(),
            nonces: HashMap::new(),
            writers: HashMap::new(),
            stopped: Default::default(),
            clock: 0,
            most,
        })
    }
}

/// The repository at `repo_path` as `cache` has it open under `name`,
/// opened where it is not or is no longer the one that is there. One of
/// the directories of `root` is opened under the name the root lists it
/// by and no other.
fn opened(
    cache: &Held<Opened>,
    name: &str,
    repo_path: PathBuf,
    root: Option<&std::path::Path>,
) -> Result<RepoHandle, Box<dyn std::error::Error>> {
    let (dir, open) = readers::directory(&repo_path)?;
    if let Some(handle) = cache.take().known(name, dir) {
        return Ok(handle);
    }
    if let Some(root) = root
        && !readers::listed(root, name)?
    {
        return Err(format!("repo '{name}' not found under server root").into());
    }
    // opened with the cache let go of: an open that waits for a lock of
    // this repository keeps no request for another waiting. The probe
    // tells what is no repository here, and not on every request
    Repository::discover(&repo_path)?;
    let alt_dir = repo_path.join(".alt");
    let known = cache.take().writer(name, dir);
    let writer = match (alt_dir.is_dir(), known) {
        (true, None) => {
            let nonces = cache.take().nonces(name, dir);
            let (of, stopped) = ((name.to_owned(), dir), Arc::clone(&cache.take().stopped));
            let opened = open_write_coordinator(alt_dir, nonces, open.try_clone()?, of, stopped)?;
            Some(cache.take().written(name, dir, opened))
        }
        (_, known) => known,
    };
    // the first to have opened it is the one every request uses
    let opened = RepoHandle::new(repo_path, writer, &readers::none());
    Ok(cache.take().keep(name, (dir, open), opened))
}

impl ServeMode {
    pub(super) fn single(repo_path: PathBuf) -> Self {
        // Fail-fast: a misconfigured ALT_SERVER_REPO surfaces at boot
        // rather than on the first request.
        let cache = Opened::of(1);
        let handle = opened(&cache, "*", repo_path.clone(), None);
        let handle = handle.unwrap_or_else(|e| die(&format!("opening repo: {e}")));
        if handle.writer.is_none() {
            eprintln!(
                "altd-server: no .alt under {p}; receive-pack will be refused (read-only mode)",
                p = repo_path.display()
            );
        }
        ServeMode::Single { repo_path, cache }
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
        // a users file that does not read is told now, not to each request
        let users_path = root.join("users");
        let users = users_path.is_file();
        if users && let Err(why) = auth::users_read(&users_path) {
            die(&format!("{}: {why}", users_path.display()));
        }
        let cache = Opened::of(told::told().repos);
        ServeMode::Multi { root, cache, users }
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
        ServeMode::Single { repo_path, cache } => {
            let handle = opened(cache, "*", repo_path.clone(), None)?;
            Ok((handle, path.to_owned(), "*".to_owned()))
        }
        ServeMode::Multi { root, cache, .. } => {
            let trimmed = path.trim_start_matches('/');
            let (name, rest) = match trimmed.find('/') {
                Some(i) => (&trimmed[..i], &trimmed[i..]),
                None => (trimmed, ""),
            };
            let handle = opened(cache, name, readers::named(root, name)?, Some(root))?;
            Ok((handle, rest.to_owned(), name.to_owned()))
        }
    }
}

#[cfg(test)]
#[path = "mode/tests.rs"]
mod tests;
#[path = "mode/writer.rs"]
mod writer;
pub(super) use writer::{WriteCoordinator, open_write_coordinator};
