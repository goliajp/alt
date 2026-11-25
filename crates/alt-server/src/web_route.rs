use alt_git_codec::ObjectId;
use alt_remote_protocol::{RecordKind, RecordRoot, RequestKey, Scope};
use alt_server_store::{Error, Result};
use std::collections::BTreeMap;

mod browse;
mod repository;

/// Where an object is shown: under a commit, at a path of its tree. The path is
/// in hex as a page of a directory tells it, and empty for the tree of the commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Within {
    pub commit: ObjectId,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebRoute {
    Portable(crate::PortableRoute),
    ChangeLink(crate::change_links::route::ChangeRoute),
    Config,
    InstanceStatus,
    InstanceAudit {
        before: Option<u64>,
    },
    Login,
    Session,
    Navigation,
    Sessions,
    RevokeSession(String),
    DeviceConsent(String),
    DeviceDecision,
    ClientCredentials,
    AgentDelegations(String),
    RevokeAgent(String),
    RevokeClientCredential(String),
    Logout,
    Repositories {
        after: String,
    },
    Inbox {
        after: String,
    },
    Users {
        after: String,
    },
    CreateUser,
    UserState(String),
    Repository(String),
    Records(String, RecordKind, String),
    Record(String, RecordRoot),
    History(String, Option<ObjectId>),
    Tree(String, ObjectId, String, usize),
    /// an object, where it is shown, and whether it is downloaded
    Object(String, ObjectId, Within, bool),
    Content(String, String),
    Review(RequestKey),
    Approve(RequestKey),
    ReviewContent(RequestKey, String),
}
fn identity(value: &str) -> Result<String> {
    Scope {
        installation_id: value.into(),
        repository_instance_id: "route".into(),
        recovery_epoch: 1,
    }
    .validate()?;
    Ok(value.into())
}
fn id(value: &str) -> Result<ObjectId> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Precondition("object address"));
    }
    value
        .parse()
        .map_err(|_| Error::Precondition("object address"))
}
fn record_kind(value: &str) -> Result<RecordKind> {
    alt_json::from_value(alt_json::Value::String(value.into()))
        .map_err(|_| Error::Precondition("record kind"))
}
fn arguments(query: Option<&str>, allowed: &[&str]) -> Result<BTreeMap<String, String>> {
    let query = query.unwrap_or("");
    if query.len() > 16_384 {
        return Err(Error::Limit("query bytes"));
    }
    let mut result = BTreeMap::new();
    for (key, value) in alt_url::form::parse(query.as_bytes()) {
        if !allowed.contains(&key.as_ref())
            || result
                .insert(key.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(Error::Precondition("query arguments"));
        }
    }
    Ok(result)
}
pub fn parse(
    method: &str,
    path: &str,
    query: Option<&str>,
    installation: &str,
) -> Result<WebRoute> {
    if path.len() > 2048 || path.contains('%') || path.contains('\\') || !path.is_ascii() {
        return Err(Error::Precondition("web route"));
    }
    if let Some(tail) = path.strip_prefix("/web/")
        && let Some(crate::Route::Portable(route)) =
            crate::parse_route(method, &format!("/v1/{tail}"), query, installation)
    {
        return Ok(WebRoute::Portable(route));
    }
    if let Some(tail) = path.strip_prefix("/web/")
        && let Some(crate::Route::ChangeLink(
            route @ crate::change_links::route::ChangeRoute::Scoped(_, _),
        )) = crate::parse_route(method, &format!("/v1/{tail}"), query, installation)
    {
        return Ok(WebRoute::ChangeLink(route));
    }
    let parts = path.split('/').collect::<Vec<_>>();
    let none = || arguments(query, &[]).map(|_| ());
    let after =
        || arguments(query, &["after"]).map(|mut args| args.remove("after").unwrap_or_default());
    Ok(match (method, parts.as_slice()) {
        ("GET", ["", "web", "instance"]) => {
            none()?;
            WebRoute::InstanceStatus
        }
        ("GET", ["", "web", "instance", "audit"]) => {
            let value = after()?;
            let before = if value.is_empty() {
                None
            } else {
                let number = value
                    .parse::<u64>()
                    .map_err(|_| Error::Precondition("audit cursor"))?;
                if number == 0 || number.to_string() != value {
                    return Err(Error::Precondition("audit cursor"));
                }
                Some(number)
            };
            WebRoute::InstanceAudit { before }
        }
        ("GET", ["", "web", "config"]) => {
            none()?;
            WebRoute::Config
        }
        ("POST", ["", "web", "login"]) => {
            none()?;
            WebRoute::Login
        }
        ("GET", ["", "web", "navigation"]) => {
            none()?;
            WebRoute::Navigation
        }
        ("GET", ["", "web", "session"]) => {
            none()?;
            WebRoute::Session
        }
        ("GET", ["", "web", "sessions"]) => {
            none()?;
            WebRoute::Sessions
        }
        ("GET", ["", "web", "device"]) => {
            let mut args = arguments(query, &["user_code"])?;
            let code = args
                .remove("user_code")
                .ok_or(Error::Precondition("user code required"))?;
            WebRoute::DeviceConsent(alt_remote_protocol::login::user_code(&code)?)
        }
        ("POST", ["", "web", "device"]) => {
            none()?;
            WebRoute::DeviceDecision
        }
        ("GET", ["", "web", "delegations"]) => WebRoute::AgentDelegations(after()?),
        ("POST", ["", "web", "delegations", id, "revoke"]) => {
            none()?;
            alt_task::check_id(id).map_err(|_| Error::Precondition("credential id"))?;
            WebRoute::RevokeAgent((*id).into())
        }
        ("GET", ["", "web", "client-credentials"]) => {
            none()?;
            WebRoute::ClientCredentials
        }
        ("POST", ["", "web", "client-credentials", id, "revoke"]) => {
            none()?;
            alt_task::check_id(id).map_err(|_| Error::Precondition("credential id"))?;
            WebRoute::RevokeClientCredential((*id).into())
        }
        ("POST", ["", "web", "sessions", id, "revoke"]) => {
            none()?;
            alt_task::check_id(id).map_err(|_| Error::Precondition("session id"))?;
            WebRoute::RevokeSession((*id).into())
        }
        ("POST", ["", "web", "logout"]) => {
            none()?;
            WebRoute::Logout
        }
        ("GET", ["", "web", "repositories"]) => WebRoute::Repositories { after: after()? },
        ("GET", ["", "web", "inbox"]) => WebRoute::Inbox { after: after()? },
        ("GET", ["", "web", "users"]) => WebRoute::Users { after: after()? },
        ("POST", ["", "web", "users"]) => {
            none()?;
            WebRoute::CreateUser
        }
        ("POST", ["", "web", "users", actor, "state"]) => {
            none()?;
            WebRoute::UserState(identity(actor)?)
        }
        (method, ["", "web", "repositories", repo, rest @ ..]) => {
            repository::parse(method, repo, rest, query, installation)?
        }
        _ => return Err(Error::Precondition("web route")),
    })
}
