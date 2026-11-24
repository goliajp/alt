//! The routes that page through what a repository holds.
use super::{Error, Result, WebRoute, arguments, id, identity};

/// the page of history that begins at `cursor`, or at the default branch
pub(super) fn history(repository: &str, cursor: String) -> Result<WebRoute> {
    let after = if cursor.is_empty() {
        None
    } else {
        Some(id(&cursor)?)
    };
    Ok(WebRoute::History(identity(repository)?, after))
}

/// a page of a directory: `after` is how many of its entries the pages before held
pub(super) fn tree(repository: &str, query: Option<&str>) -> Result<WebRoute> {
    let mut args = arguments(query, &["commit", "path", "after"])?;
    let after = match args.remove("after") {
        Some(after) if !after.is_empty() => after
            .parse()
            .map_err(|_| Error::Precondition("tree page"))?,
        _ => 0,
    };
    let commit = args
        .remove("commit")
        .ok_or(Error::Precondition("commit required"))?;
    Ok(WebRoute::Tree(
        identity(repository)?,
        id(&commit)?,
        args.remove("path").unwrap_or_default(),
        after,
    ))
}
