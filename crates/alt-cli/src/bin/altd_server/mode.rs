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
        cache: Held<HashMap<String, RepoHandle>>,
    },
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

/// In-memory single-use nonce table. Cap at 1024 active entries; on
/// overflow the oldest is dropped (LRU FIFO). This is a tradeoff:
/// under burst load an honest client whose info/refs nonce ages out
/// before they POST receive-pack gets refused, but the bound makes
/// the table O(1) memory regardless of traffic.
pub(super) struct NonceTable {
    pub(super) inner: Held<NonceInner>,
}

pub(super) struct NonceInner {
    pub(super) queue: std::collections::VecDeque<String>,
    pub(super) set: std::collections::HashSet<String>,
}

impl NonceTable {
    pub(super) fn new() -> Self {
        NonceTable {
            inner: Held::new(NonceInner {
                queue: std::collections::VecDeque::with_capacity(1024),
                set: std::collections::HashSet::with_capacity(1024),
            }),
        }
    }

    /// Issue a fresh 32-char-hex nonce (128 bits — collision negligible
    /// against any realistic traffic + 1024-entry table). Inserts it
    /// into the table; on overflow the oldest entry is evicted.
    pub(super) fn issue(&self) -> String {
        let nonce = {
            // 16 bytes from the OS entropy pool via /dev/urandom; we
            // avoid a `rand` dep by reading directly. Failure to read
            // is essentially "running on Mars" — fall back to a
            // timestamp-derived value so the path doesn't crash.
            let mut buf = [0u8; 16];
            match std::fs::File::open("/dev/urandom") {
                Ok(mut f) => {
                    use std::io::Read;
                    let _ = f.read_exact(&mut buf);
                }
                Err(_) => {
                    let ms = unix_ms_now().to_le_bytes();
                    buf[..ms.len()].copy_from_slice(&ms);
                }
            }
            let mut hex = String::with_capacity(32);
            for b in buf {
                hex.push_str(&format!("{b:02x}"));
            }
            hex
        };
        let mut g = self.inner.take();
        if g.queue.len() >= 1024
            && let Some(old) = g.queue.pop_front()
        {
            g.set.remove(&old);
        }
        g.queue.push_back(nonce.clone());
        g.set.insert(nonce.clone());
        nonce
    }

    /// Atomically check + consume a nonce. Returns true iff it was in
    /// the table at the time of call; subsequent calls return false.
    pub(super) fn consume(&self, nonce: &str) -> bool {
        let mut g = self.inner.take();
        if g.set.remove(nonce) {
            // also drop it from the queue so the LRU bookkeeping stays
            // accurate; we accept the O(N) here because consumes are
            // rare relative to issues.
            if let Some(pos) = g.queue.iter().position(|s| s == nonce) {
                g.queue.remove(pos);
            }
            true
        } else {
            false
        }
    }
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
            cache: Held::new(HashMap::new()),
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
            if let Some(h) = cache.take().get(name) {
                return Ok((h.clone(), rest.to_owned(), name.to_owned()));
            }
            // opened with the cache let go of: an open that waits for a
            // lock of this repository keeps no request for another waiting
            let repo_path = readers::named(root, name)?;
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
            let handle = cache
                .take()
                .entry(name.to_owned())
                .or_insert(opened)
                .clone();
            Ok((handle, rest.to_owned(), name.to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the table issues a nonce that consumes exactly once;
    /// the second consume on the same value returns false. That's
    /// the single-use anti-replay primitive.
    #[test]
    fn nonce_table_consume_is_single_use() {
        let t = NonceTable::new();
        let n = t.issue();
        assert_eq!(n.len(), 32, "issued nonce should be 32 hex chars");
        assert!(t.consume(&n), "first consume must succeed");
        assert!(!t.consume(&n), "second consume on the same nonce must fail");
    }

    /// A nonce that was never issued can't be consumed. Stops a
    /// confused-deputy that tries to pass off a guessed value as
    /// if it had been negotiated.
    #[test]
    fn nonce_table_rejects_never_issued_value() {
        let t = NonceTable::new();
        assert!(
            !t.consume("00000000000000000000000000000000"),
            "consuming a never-issued nonce must fail"
        );
    }

    /// At cap (1024 entries), issuing one more evicts the oldest;
    /// after eviction the evicted value can't be consumed. This
    /// bounds memory regardless of traffic at the cost of refusing
    /// an honest client whose info/refs nonce aged out before they
    /// posted receive-pack.
    #[test]
    fn nonce_table_evicts_oldest_when_full() {
        let t = NonceTable::new();
        let first = t.issue();
        for _ in 1..1024 {
            let _ = t.issue();
        }
        // table is full now; one more issue must evict `first`.
        let _last = t.issue();
        assert!(
            !t.consume(&first),
            "the evicted (oldest) nonce must no longer be consumable"
        );
    }
}
