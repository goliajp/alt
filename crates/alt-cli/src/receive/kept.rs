//! Commits read while pushes are looked at, kept from one push to the
//! next.
//!
//! A push beside the refs has every commit a ref points at read, to
//! tell which of the commits under it the refs reach: with ten thousand
//! refs that is ten thousand objects taken out of the store for each
//! such push, the same ones every time. A commit is what its id says,
//! in whichever repository: its bytes are kept by its id, for all the
//! repositories this process serves, within [`LIMIT`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alt_git_codec::ObjectId;

/// The bytes of commits kept; all of them are let go of once one more
/// would not fit.
const LIMIT: usize = 32 << 20;
/// A commit larger than this is not kept.
const LARGEST: usize = 64 << 10;

#[derive(Default)]
struct Kept {
    commits: HashMap<ObjectId, Arc<Vec<u8>>>,
    bytes: usize,
}

static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

/// The commit `oid` as it was read before.
pub(super) fn commit(oid: &ObjectId) -> Option<Arc<Vec<u8>>> {
    let kept = KEPT.lock().unwrap();
    kept.as_ref()?.commits.get(oid).cloned()
}

/// Keeps `data`, which the commit `oid` was read as.
pub(super) fn keep(oid: &ObjectId, data: &[u8]) {
    if data.len() > LARGEST {
        return;
    }
    let mut kept = KEPT.lock().unwrap();
    let kept = kept.get_or_insert_with(Kept::default);
    if kept.bytes + data.len() > LIMIT {
        *kept = Kept::default();
    }
    if kept.commits.insert(*oid, Arc::new(data.to_vec())).is_none() {
        kept.bytes += data.len();
    }
}
