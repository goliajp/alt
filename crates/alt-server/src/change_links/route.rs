use crate::route::{digest, id};
use alt_remote_protocol::{RequestKey, Scope};
#[derive(Clone, Debug, PartialEq, Eq)]

pub enum ChangeRoute {
    Inbox,
    Scoped(Scope, Action),
}
#[derive(Clone, Debug, PartialEq, Eq)]

pub enum Action {
    Channels,
    Events(Option<String>),
    Event(String),
    Request(String, String),
    Source(String),
    Claim(String),
    Mapping,
    Withdraw,
    Bind,
    Reauthorize(String, String),
}
impl ChangeRoute {
    pub fn has_json_body(&self) -> bool {
        matches!(
            self,
            Self::Inbox | Self::Scoped(_, Action::Mapping | Action::Withdraw | Action::Bind)
        )
    }
    pub fn request_key(&self) -> Option<RequestKey> {
        if let Self::Scoped(scope, Action::Request(actor, request)) = self {
            Some(RequestKey {
                scope: scope.clone(),
                actor_id: actor.clone(),
                request_id: request.clone(),
            })
        } else {
            None
        }
    }
}
pub fn parse(
    method: &str,
    scope: Scope,
    tail: &[&str],
    query: Option<&str>,
) -> Option<ChangeRoute> {
    let after = match query {
        None => None,
        Some(q) => {
            let d = q.strip_prefix("after=")?;
            if !digest(d) {
                return None;
            }
            Some(d.to_string())
        }
    };
    let action = match (method, tail) {
        ("GET", ["events"]) => Action::Events(after),
        _ if query.is_some() => return None,
        ("GET", ["channels"]) => Action::Channels,
        ("GET", ["events", d]) if digest(d) => Action::Event((*d).into()),
        ("GET", ["requests", actor, request]) if id(actor) && id(request) => {
            Action::Request((*actor).into(), (*request).into())
        }
        ("GET", ["repairs", d, "source"]) if digest(d) => Action::Source((*d).into()),
        ("GET", ["repairs", d, "task"]) if digest(d) => Action::Claim((*d).into()),
        ("POST", ["mappings"]) => Action::Mapping,
        ("POST", ["withdrawals"]) => Action::Withdraw,
        ("POST", ["task-bindings"]) => Action::Bind,
        ("POST", ["channels", channel, "events", d, "reauthorize"]) if id(channel) && digest(d) => {
            Action::Reauthorize((*channel).into(), (*d).into())
        }
        _ => return None,
    };
    Some(ChangeRoute::Scoped(scope, action))
}
