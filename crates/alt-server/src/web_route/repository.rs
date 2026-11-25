//! The routes beneath one repository.
use super::{
    Error, RecordRoot, RequestKey, Result, Scope, WebRoute, arguments, browse, identity,
    record_kind,
};

/// the request of `actor` that the path names, in the epoch it names
fn request(
    installation: &str,
    repository: &str,
    epoch: &str,
    actor: &str,
    request: &str,
) -> Result<RequestKey> {
    let number = epoch
        .parse::<u64>()
        .map_err(|_| Error::Precondition("request epoch"))?;
    if number.to_string() != *epoch {
        return Err(Error::Precondition("canonical epoch"));
    }
    let key = RequestKey {
        scope: Scope {
            installation_id: installation.into(),
            repository_instance_id: identity(repository)?,
            recovery_epoch: number,
        },
        actor_id: identity(actor)?,
        request_id: identity(request)?,
    };
    key.validate()?;
    Ok(key)
}

/// The route `rest` names beneath the repository `repo`.
pub(super) fn parse(
    method: &str,
    repo: &str,
    rest: &[&str],
    query: Option<&str>,
    installation: &str,
) -> Result<WebRoute> {
    let none = || arguments(query, &[]).map(|_| ());
    let after =
        || arguments(query, &["after"]).map(|mut args| args.remove("after").unwrap_or_default());
    Ok(match (method, rest) {
        ("GET", []) => {
            none()?;
            WebRoute::Repository(identity(repo)?)
        }
        ("GET", ["records", kind]) => {
            WebRoute::Records(identity(repo)?, record_kind(kind)?, after()?)
        }
        ("GET", ["records", kind, record]) => {
            none()?;
            let root = RecordRoot {
                kind: record_kind(kind)?,
                id: (*record).into(),
            };
            root.validate()?;
            WebRoute::Record(identity(repo)?, root)
        }
        ("GET", ["commits"]) => browse::history(repo, after()?)?,
        ("GET", ["tree"]) => browse::tree(repo, query)?,
        ("GET", ["objects", oid]) => browse::object(repo, oid, query, false)?,
        ("GET", ["objects", oid, "download"]) => browse::object(repo, oid, query, true)?,
        ("GET", ["content", digest, "download"]) => {
            none()?;
            alt_task::check_id(digest).map_err(|_| Error::Precondition("content address"))?;
            WebRoute::Content(identity(repo)?, (*digest).into())
        }
        (
            "GET",
            [
                "epochs",
                epoch,
                "requests",
                actor,
                name,
                "content",
                digest,
                "download",
            ],
        ) => {
            none()?;
            alt_task::check_id(digest).map_err(|_| Error::Precondition("content address"))?;
            let key = request(installation, repo, epoch, actor, name)?;
            WebRoute::ReviewContent(key, (*digest).into())
        }
        (method, ["epochs", epoch, "requests", actor, name, action]) => {
            none()?;
            let key = request(installation, repo, epoch, actor, name)?;
            match (method, *action) {
                ("GET", "review") => WebRoute::Review(key),
                ("POST", "approve") => WebRoute::Approve(key),
                _ => return Err(Error::Precondition("web request action")),
            }
        }
        _ => return Err(Error::Precondition("web route")),
    })
}
