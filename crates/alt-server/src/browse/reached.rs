//! Which commits the pages of a repository begin at.
use crate::git_packs::raw;
use alt_git_codec::ObjectId;
use alt_server_store::{Credential, Error, Result, Store};
use std::sync::atomic::AtomicBool;

/// Holds `commit` to what the refs the caller reads lead to: what one of them
/// points at, or a commit below one. A commit that is in the segments and
/// under no ref, of a branch that was deleted or of history that was replaced,
/// is not shown to whoever knows its id. In a repository that had nothing
/// deleted or replaced every commit of the segments is led to; in one that had,
/// what the refs lead to is the one the other routes keep, found once for a
/// page and the next, and left once the request is waited for no longer.
pub(super) fn led_to(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    (commit, gone): (ObjectId, Option<&AtomicBool>),
) -> Result<()> {
    let view = store.pack_view(credential, repository)?;
    let commit = raw(commit);
    let led = view.segments.commit(&commit).is_some()
        && (!view.unreachable || store.led(&view, gone)?.to(&commit));
    match led {
        true => Ok(()),
        false => Err(Error::Unauthorized),
    }
}
