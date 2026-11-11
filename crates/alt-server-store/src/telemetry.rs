//! Redacted classifications of adapter work. No raw error payload is recorded.
use crate::Jsonb;
use crate::{Error, Result};
use alt_remote_protocol::{RequestKey, Scope};
use alt_server_telemetry::{Actor, Id, Outcome, Phase, PhaseGuard, Trace};

pub fn outcome(error: &Error) -> Outcome {
    match error {
        Error::Unauthorized | Error::Repository(alt_repository_service::Error::Forbidden(_)) => {
            Outcome::Denied
        }
        Error::OutcomeUnknown => Outcome::Unknown,
        Error::AuthorityUnavailable | Error::Database(_) | Error::Io(_) => Outcome::Unavailable,
        Error::Repository(alt_repository_service::Error::Precondition(_)) => Outcome::Conflict,
        Error::Precondition(_) => Outcome::Precondition,
        Error::Limit(_)
        | Error::Repository(alt_repository_service::Error::Limit(_))
        | Error::Task(alt_task_service::Error::Limit(_))
        | Error::Portable(alt_portable::Error::Limit(_)) => Outcome::Limit,
        Error::Integrity(_)
        | Error::Repository(
            alt_repository_service::Error::Integrity(_) | alt_repository_service::Error::Missing(_),
        ) => Outcome::Integrity,
        _ => Outcome::Invalid,
    }
}
pub fn observe<T>(phase: Phase, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let guard = PhaseGuard::current(phase);
    let result = work();
    guard.finish(result.as_ref().map(|_| Outcome::Ok).unwrap_or_else(outcome));
    result
}
pub fn scope(scope: &Scope) {
    if let Some(trace) = Trace::current() {
        trace.identity(Id::new(&scope.repository_instance_id), None, None);
    }
}
pub fn request(key: &RequestKey) {
    if let Some(trace) = Trace::current() {
        trace.identity(
            Id::new(&key.scope.repository_instance_id),
            Id::new(&key.request_id),
            None,
        );
    }
}
pub fn committed(receipt: &alt_remote_protocol::Receipt) {
    request(&receipt.body.publication.key);
    if let Some(trace) = Trace::current() {
        trace.identity(
            None,
            None,
            Id::new(&receipt.body.transaction_sequence.to_string()),
        );
    }
}
pub(crate) fn authenticated(row: &postgres::Row) {
    authenticated_as(&row.get::<_, Jsonb>("attribution").0);
}
pub(crate) fn authenticated_as(attribution: &alt_json::Value) {
    if let Some(trace) = Trace::current() {
        let actor = match attribution.get("kind").and_then(alt_json::Value::as_str) {
            Some("human") => Actor::Human,
            Some("agent") => Actor::Agent,
            Some("service") => Actor::Service,
            _ => Actor::Authenticated,
        };
        trace.actor(actor);
    }
}
