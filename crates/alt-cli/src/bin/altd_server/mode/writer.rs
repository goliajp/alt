//! What a repository that is written to has besides its store.

use super::*;

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
pub(in super::super) struct WriteCoordinator {
    pub(in super::super) store: Held<Store>,
    pub(in super::super) sink: alt_cli::native::StoreSink,
    pub(in super::super) group: alt_cli::group_commit::GroupCommit,
    /// single-use nonces issued during receive-pack info/refs
    /// and consumed during the matching `git-receive-pack` POST. Bounds
    /// the replay window to "between info/refs advert and EOL of the
    /// LRU table". A captured signed push payload can only be replayed
    /// against this server if its nonce hasn't yet been consumed and
    /// hasn't aged out of the table.
    pub(in super::super) nonces: Arc<NonceTable>,
    /// the directory of the repository, open for as long as this is
    _dir: std::fs::File,
}

/// Open the write store and build the write coordinator: turn on deferred
/// durability so each commit appends-without-fsync, create the off-write
/// `sink` so the fsync uses independent fds, instantiate the
/// `GroupCommit` that coalesces concurrent flushes.
pub(in super::super) fn open_write_coordinator(
    alt_dir: std::path::PathBuf,
    nonces: Arc<NonceTable>,
    dir: std::fs::File,
) -> Result<Arc<WriteCoordinator>, Box<dyn std::error::Error>> {
    let mut store = Store::open(alt_dir)?;
    store.set_defer_durability(true);
    let sink = store.sink()?;
    Ok(Arc::new(WriteCoordinator {
        store: Held::new(store),
        sink,
        group: alt_cli::group_commit::GroupCommit::new(),
        nonces,
        _dir: dir,
    }))
}
