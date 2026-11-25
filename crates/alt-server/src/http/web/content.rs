//! A published payload asked for by the digest of its content.

use crate::Output;
use alt_server_store::{Credential, Result, Store};
use std::sync::atomic::AtomicBool;

/// The payload, to one who is shown the repository by its refs: a Git object
/// of a repository that holds what no ref leads to is given where a ref does.
/// That is looked for no longer once `gone` is set.
pub(super) fn given(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    digest: String,
    gone: &AtomicBool,
) -> Result<Output> {
    let scope = store.repository_info(credential, repository)?.scope;
    let (bytes, file) = store.open_reached_content(credential, &scope, &digest, Some(gone))?;
    Ok(Output::File {
        bytes,
        digest,
        file,
        held: None,
    })
}
