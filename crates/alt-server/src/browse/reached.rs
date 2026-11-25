//! Which commits the pages of a repository begin at.
use crate::git_packs::raw;
use alt_git_codec::ObjectId;
use alt_hash::KeyedIdSet;
use alt_pack_segment::Oid;
use alt_server_store::{Credential, Error, Result, Store};

/// Holds `commit` to what the refs the caller reads lead to: what one of them
/// points at, or a commit below one. A commit that is in the segments and
/// under no ref, of a branch that was deleted or of history that was replaced,
/// is not shown to whoever knows its id.
pub(super) fn led_to(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    commit: ObjectId,
) -> Result<()> {
    let view = store.pack_view(credential, repository)?;
    let tips: KeyedIdSet<Oid> = view.refs.values().map(raw).collect();
    if tips.contains(&raw(commit)) {
        return Ok(());
    }
    let steps = store.limits().walk_steps;
    let mut remaining = steps;
    let said = [commit];
    let found = crate::git_read::held(&view.segments, &tips, &said, &mut remaining, steps)?;
    match found.is_empty() {
        true => Err(Error::Unauthorized),
        false => Ok(()),
    }
}
