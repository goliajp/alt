//! A page of the records of a kind a repository has published.

use super::{RecordSummary, scope, summary};
use alt_remote_protocol::RecordKind;
use alt_server_store::{Credential, Error, Page, Result, Store};

/// what a record is refused with that is larger than it may be read at
const PROJECTED: &str = "record projection bytes";
pub fn list(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    kind: RecordKind,
    after: &str,
    limit: usize,
) -> Result<Page<RecordSummary>> {
    if limit == 0 || limit >= 100 {
        return Err(Error::Limit("record page size"));
    }
    let scope = scope(store, credential, repository)?;
    let roots = store.published_records(
        credential,
        &scope,
        kind,
        if after.is_empty() { None } else { Some(after) },
        limit + 1,
    )?;
    let mut more = roots.len() > limit;
    let mut items = Vec::new();
    let mut remaining = 16 * 1024 * 1024;
    for root in roots.into_iter().take(limit) {
        // a record there is no room for is the first of the next page
        let record = match store.published_record(credential, &scope, &root, remaining) {
            Err(Error::Limit(PROJECTED)) if !items.is_empty() => {
                more = true;
                break;
            }
            record => record?,
        };
        remaining -= record.payload.bytes as usize;
        items.push(summary(&root, &record));
    }
    let next = if more {
        items.last().map(|v| v.id.clone())
    } else {
        None
    };
    Ok(Page { items, next })
}
