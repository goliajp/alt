//! The routes that page through what a repository holds.
use super::{Error, Result, WebRoute, Within, arguments, id, identity};

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

/// an object by its id, under the commit and at the path it is shown at
pub(super) fn object(
    repository: &str,
    oid: &str,
    query: Option<&str>,
    download: bool,
) -> Result<WebRoute> {
    let mut args = arguments(query, &["commit", "path"])?;
    let commit = args
        .remove("commit")
        .ok_or(Error::Precondition("commit required"))?;
    let within = Within {
        commit: id(&commit)?,
        path: args.remove("path").unwrap_or_default(),
    };
    Ok(WebRoute::Object(
        identity(repository)?,
        id(oid)?,
        within,
        download,
    ))
}
