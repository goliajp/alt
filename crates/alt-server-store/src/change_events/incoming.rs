//! What is made of an event a peer sent: accepted, refused, or known already.

use super::*;

pub(super) fn incoming_decision(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    channel: &ChangeChannel,
    current: &RepositorySnapshot,
    auth: &Authorization,
    event: &link::Event,
) -> Result<(&'static str, link::Decision)> {
    use link::{Body, Decision, Refusal};
    match &event.body {
        Body::RepairRequest(request) => {
            if !event.entity.starts_with("repair-") {
                return Err(Error::Precondition("change-link repair entity"));
            }
            if request
                .observed_version
                .application
                .as_ref()
                .is_some_and(|v| v != &channel.application)
                || !current.refs.contains_key(&request.target_ref)
                || !auth
                    .grants
                    .iter()
                    .any(|g| g.update && g.refs.matches(&request.target_ref))
            {
                return Ok((
                    "repair_request",
                    Decision::Refused(Refusal::TargetUnavailable),
                ));
            }
            if let Some(digest) = &request.mapping_event_digest {
                let mapping = accepted_mapping(tx, scope, &channel.channel, digest)?;
                let matches = mapping.is_some_and(|m| {
                    request
                        .observed_version
                        .application
                        .as_ref()
                        .is_none_or(|v| v == &m.release.application)
                        && request
                            .observed_version
                            .release
                            .as_ref()
                            .is_none_or(|v| v == &m.release.release)
                        && request
                            .observed_version
                            .release_name
                            .as_ref()
                            .is_none_or(|v| v == &m.release.release_name)
                        && request
                            .observed_version
                            .environment
                            .as_ref()
                            .is_none_or(|v| v == &m.release.environment)
                });
                if !matches {
                    return Ok(("repair_request", Decision::Refused(Refusal::StateConflict)));
                }
            }
            Ok(("repair_request", Decision::Applied))
        }
        Body::Observation(observation) => {
            if event.entity != format!("observation-{}", observation.mapping_event_digest) {
                return Err(Error::Precondition("change-link observation entity"));
            }
            let mapping = accepted_mapping(
                tx,
                scope,
                &channel.channel,
                &observation.mapping_event_digest,
            )?;
            let decision = if mapping.is_some_and(|m| {
                m.release == observation.release && m.release.application == channel.application
            }) {
                Decision::Applied
            } else {
                Decision::Refused(Refusal::StateConflict)
            };
            Ok(("observation", decision))
        }
        _ => Err(Error::Precondition("change-link incoming direction")),
    }
}
