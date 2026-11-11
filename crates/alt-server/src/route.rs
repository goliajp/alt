use alt_remote_protocol::{RecordKind, RecordRoot, RequestKey, Scope};

#[derive(Clone, Debug, PartialEq, Eq)]

pub enum RequestAction {
    Query,
    Upload(String),
    UploadChunk(String),
    UploadStatus,
    Assemble(String),
    Prepare,
    Publish,
    Approve,
    Review,
    ReviewContent(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]

pub enum Route {
    ChangeLink(crate::change_links::route::ChangeRoute),
    Portable(crate::PortableRoute),
    Health,
    Metrics,
    Capabilities,
    Principal,
    Delegations(Option<String>),
    IssueAgent,
    Delegation(String),
    RevokeAgent(String),
    Repository(String),
    Begin(Scope),
    Request(RequestKey, RequestAction),
    Content(Scope, String),
    Bundles(Scope, RecordRoot, Option<String>),
    Records(Scope, RecordKind, Option<String>),
    GitAdvertise(String, bool),
    GitUpload(String),
    GitReceive(String),
}
pub(crate) fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub(crate) fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn record_kind(value: &str) -> Option<RecordKind> {
    Some(match value {
        "task" => RecordKind::Task,
        "candidate" => RecordKind::Candidate,
        "evidence" => RecordKind::Evidence,
        "review" => RecordKind::Review,
        "integration" => RecordKind::Integration,
        "delivery" => RecordKind::Delivery,
        _ => return None,
    })
}
fn after(query: Option<&str>) -> Option<Option<String>> {
    match query {
        None => Some(None),
        Some(query) => {
            let value = query.strip_prefix("after=")?;
            if !digest(value) {
                return None;
            }
            Some(Some(value.into()))
        }
    }
}
/// Opaque IDs never become filesystem paths. Percent encoding, extra query
/// parameters, duplicate separators and noncanonical epochs are rejected.
pub fn parse_route(
    method: &str,
    path: &str,
    query: Option<&str>,
    installation: &str,
) -> Option<Route> {
    if path.len() > 2048 || path.contains('%') || path.contains('\\') || !path.is_ascii() {
        return None;
    }
    let p: Vec<_> = path.split('/').collect();
    match (method, p.as_slice(), query) {
        ("GET", ["", "git", repository, "info", "refs"], Some("service=git-upload-pack"))
            if id(repository) =>
        {
            return Some(Route::GitAdvertise((*repository).into(), false));
        }
        ("GET", ["", "git", repository, "info", "refs"], Some("service=git-receive-pack"))
            if id(repository) =>
        {
            return Some(Route::GitAdvertise((*repository).into(), true));
        }
        ("POST", ["", "git", repository, "git-upload-pack"], None) if id(repository) => {
            return Some(Route::GitUpload((*repository).into()));
        }
        ("POST", ["", "git", repository, "git-receive-pack"], None) if id(repository) => {
            return Some(Route::GitReceive((*repository).into()));
        }
        _ => {}
    }
    if method == "GET" && path == "/v1/delegations" {
        return Some(Route::Delegations(after(query)?));
    }
    if query.is_none() {
        match (method, p.as_slice()) {
            ("POST", ["", "api", "v1", "change-links", "inbox"]) => {
                return Some(Route::ChangeLink(
                    crate::change_links::route::ChangeRoute::Inbox,
                ));
            }
            ("POST", ["", "v1", "delegations"]) => return Some(Route::IssueAgent),
            ("GET", ["", "v1", "delegations", value]) if digest(value) => {
                return Some(Route::Delegation((*value).into()));
            }
            ("POST", ["", "v1", "delegations", value, "revoke"]) if digest(value) => {
                return Some(Route::RevokeAgent((*value).into()));
            }
            ("GET", ["", "v1", "portable", "capabilities"]) => {
                return Some(Route::Portable(crate::PortableRoute::Capabilities));
            }
            ("GET", ["", "v1", "instance", "metrics"]) => return Some(Route::Metrics),
            ("GET", ["", "healthz"]) => return Some(Route::Health),
            ("GET", ["", "v1", "capabilities"]) => return Some(Route::Capabilities),
            ("GET", ["", "v1", "principal"]) => return Some(Route::Principal),
            ("GET", ["", "v1", "repositories", repository]) if id(repository) => {
                return Some(Route::Repository((*repository).into()));
            }
            _ => {}
        }
    }
    if p.len() < 7 || p[..3] != ["", "v1", "repositories"] || p[4] != "epochs" || !id(p[3]) {
        return None;
    }
    let epoch = p[5].parse::<u64>().ok()?;
    if epoch == 0 || epoch.to_string() != p[5] {
        return None;
    }
    let scope = Scope {
        installation_id: installation.into(),
        repository_instance_id: p[3].into(),
        recovery_epoch: epoch,
    };
    if scope.validate().is_err() {
        return None;
    }
    match (method, &p[6..]) {
        (_, ["changes", tail @ ..]) => {
            crate::change_links::route::parse(method, scope, tail, query).map(Route::ChangeLink)
        }
        (_, ["portable", tail @ ..]) if query.is_none() => {
            crate::portable::route::parse(method, scope, tail).map(Route::Portable)
        }
        ("GET", ["tasks", kind]) => Some(Route::Records(scope, record_kind(kind)?, after(query)?)),
        ("POST", ["requests"]) if query.is_none() => Some(Route::Begin(scope)),
        ("GET", ["content", content]) if query.is_none() && digest(content) => {
            Some(Route::Content(scope, (*content).into()))
        }
        ("GET", ["tasks", kind, record]) if digest(record) => {
            let kind = match *kind {
                "task" => RecordKind::Task,
                "candidate" => RecordKind::Candidate,
                "evidence" => RecordKind::Evidence,
                "review" => RecordKind::Review,
                "integration" => RecordKind::Integration,
                "delivery" => RecordKind::Delivery,
                _ => return None,
            };
            let after = match query {
                None => None,
                Some(q) => {
                    let value = q.strip_prefix("after=")?;
                    if !digest(value) {
                        return None;
                    }
                    Some(value.into())
                }
            };
            Some(Route::Bundles(
                scope,
                RecordRoot {
                    kind,
                    id: (*record).into(),
                },
                after,
            ))
        }
        (_, ["requests", actor, request, rest @ ..])
            if query.is_none() && id(actor) && id(request) =>
        {
            let key = RequestKey {
                scope,
                actor_id: (*actor).into(),
                request_id: (*request).into(),
            };
            let action = match (method, rest) {
                ("GET", []) => RequestAction::Query,
                ("PUT", ["content", content]) if digest(content) => {
                    RequestAction::Upload((*content).into())
                }
                ("PUT", ["chunks", chunk]) if digest(chunk) => {
                    RequestAction::UploadChunk((*chunk).into())
                }
                ("GET", ["uploads"]) => RequestAction::UploadStatus,
                ("POST", ["content", content, "assemble"]) if digest(content) => {
                    RequestAction::Assemble((*content).into())
                }
                ("POST", ["prepare"]) => RequestAction::Prepare,
                ("POST", ["publish"]) => RequestAction::Publish,
                ("POST", ["approve"]) => RequestAction::Approve,
                ("GET", ["review"]) => RequestAction::Review,
                ("GET", ["review", "content", content]) if digest(content) => {
                    RequestAction::ReviewContent((*content).into())
                }
                _ => return None,
            };
            Some(Route::Request(key, action))
        }
        _ => None,
    }
}
