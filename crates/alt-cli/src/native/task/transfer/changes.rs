//! Exact, privately saved product association intents. Sending never fabricates
//! a Delivery identity; the server derives it from committed native history.
use super::*;
use alt_remote_protocol::change_links::{ChangeMappingRequest, ChangeWithdrawalRequest};
use golia_change_link as link;
mod repair;

#[derive(alt_json::FromJson)]
#[cfg_attr(test, derive(alt_json::ToJson))]
#[json(deny_unknown_fields)]
struct Plan {
    channel: String,
    action: Action,
}
#[derive(alt_json::FromJson)]
#[cfg_attr(test, derive(alt_json::ToJson))]
#[json(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Mapping {
        delivery_id: String,
        #[json(with = "alt_json_serde::bridge")]
        release: link::Release,
        expected_previous: Option<String>,
    },
    Withdraw {
        mapping_event_digest: String,
        expected_previous: String,
        reason: String,
    },
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Mapping(ChangeMappingRequest),
    Withdraw(ChangeWithdrawalRequest),
}
impl Request {
    fn key(&self) -> &protocol::RequestKey {
        match self {
            Self::Mapping(r) => &r.key,
            Self::Withdraw(r) => &r.key,
        }
    }
    fn channel(&self) -> &str {
        match self {
            Self::Mapping(r) => &r.channel,
            Self::Withdraw(r) => &r.channel,
        }
    }
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct Saved {
    version: u32,
    binding: Binding,
    request: Request,
}
fn checked(saved: &Saved, value: &link::HistoricalEvent) -> Res<()> {
    value.validate()?;
    let event = &value.statement.event;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes(&saved.binding.receipt_key)?)?;
    value.statement.verify(
        &key,
        saved.request.channel(),
        &event.issuer,
        &event.receiver,
    )?;
    if event.issuer.product != link::Product::Alt
        || event.issuer.installation != saved.binding.scope.installation_id
        || event.issuer.scope != saved.binding.scope.repository_instance_id
        || event.issuer.generation.0 != saved.binding.scope.recovery_epoch
    {
        return Err(refused(
            "association response belongs to another server scope",
        ));
    }
    let matches = match (&saved.request, &event.body) {
        (Request::Mapping(r), link::Body::Mapping(m)) => {
            m.delivery.delivery == r.delivery_id
                && m.release == r.release
                && m.declared_by == r.key.actor_id
                && event.previous_digest == r.expected_previous
        }
        (
            Request::Withdraw(r),
            link::Body::Withdraw {
                previous_mapping_digest,
                reason,
                declared_by,
            },
        ) => {
            previous_mapping_digest == &r.mapping_event_digest
                && reason == &r.reason
                && declared_by == &r.key.actor_id
                && event.previous_digest.as_deref() == Some(&r.expected_previous)
        }
        _ => false,
    };
    if !matches {
        return Err(refused(
            "association response differs from the saved request",
        ));
    }
    Ok(())
}
fn result(
    id: &str,
    value: Option<link::HistoricalEvent>,
    json_mode: bool,
    out: &mut impl Write,
) -> Res<()> {
    let state = match value.as_ref().and_then(|v| v.acknowledgement.as_ref()) {
        Some(a) if a.receipt.body.decision == link::Decision::Applied => "delivered",
        Some(_) => "refused",
        None if value.is_some() => "queued",
        None => "not_seen",
    };
    emit(
        out,
        json_mode,
        json!({"change_id":id,"state":state,"history":alt_json_serde::Serde(value)}),
    )
}
fn failed(
    id: &str,
    key: &protocol::RequestKey,
    error: impl std::fmt::Display,
) -> Box<dyn std::error::Error> {
    let mut failure = crate::machine::Failure::unknown(error.to_string());
    failure.details = Box::new(json!({"change_id":id,"remote_request":key}));
    failure.next_action =
        "Run `alt task remote changes status <change-id>` before resuming the same association";
    failure.into()
}
impl NativeRepo<'_> {
    pub(super) fn change_command(
        &mut self,
        op: &crate::cli::ChangeOp,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use crate::cli::ChangeOp;
        match op {
            ChangeOp::RepairCreate { .. } => self.repair_create(op, json_mode, out),
            ChangeOp::Send { id } | ChangeOp::Status { id } => {
                self.change_send(id, matches!(op, ChangeOp::Status { .. }), json_mode, out)
            }
            ChangeOp::Preview { remote, plan } => self.change_preview(remote, plan, json_mode, out),
            ChangeOp::Channels { remote }
            | ChangeOp::List { remote, .. }
            | ChangeOp::Show { remote, .. }
            | ChangeOp::Repair { remote, .. }
            | ChangeOp::Reauthorize { remote, .. } => {
                let binding = self.remote_binding(remote)?;
                let client = transport(&binding)?;
                check_connection(
                    &client,
                    &binding,
                    matches!(op, ChangeOp::Reauthorize { .. }),
                )?;
                let value = match op {
                    ChangeOp::Channels { .. } => {
                        json!({"channels":client.change_channels(&binding.scope)?})
                    }
                    ChangeOp::List { after, .. } => {
                        alt_json::to_value(client.change_events(&binding.scope, after.as_deref())?)?
                    }
                    ChangeOp::Show { digest, .. } => {
                        json!({"history":alt_json_serde::Serde(client.change_event(&binding.scope,digest)?)})
                    }
                    ChangeOp::Repair { digest, .. } => {
                        let claim = client.repair_claim(&binding.scope, digest)?;
                        // Published claims remain inspectable after pairing is
                        // disabled. Current source permission is needed only
                        // when preparing a new local Task.
                        let source = if claim.is_none() {
                            Some(client.repair_source(
                                &binding.scope,
                                digest,
                                &key_bytes(&binding.receipt_key)?,
                            )?)
                        } else {
                            None
                        };
                        json!({"repair_request_digest":digest,"published_task":claim,"source":alt_json_serde::Serde(source)})
                    }
                    ChangeOp::Reauthorize {
                        channel, digest, ..
                    } => {
                        json!({"history":alt_json_serde::Serde(client.change_reauthorize(&binding.scope,channel,digest)?)})
                    }
                    _ => unreachable!(),
                };
                emit(out, json_mode, value)
            }
        }
    }
    fn change_preview(
        &mut self,
        remote: &str,
        plan: &Path,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task remote changes preview")?;
        let binding = self.remote_binding(remote)?;
        let client = transport(&binding)?;
        check_connection(&client, &binding, true)?;
        let plan: Plan = json_file(&self.root.join(plan))?;
        let channel = client
            .change_channels(&binding.scope)?
            .into_iter()
            .find(|c| c.channel == plan.channel && c.enabled)
            .ok_or_else(|| refused("Sentori channel is not currently enabled"))?;
        let key = protocol::RequestKey {
            scope: binding.scope.clone(),
            actor_id: binding.actor.actor_id.clone(),
            request_id: self.task_request()?,
        };
        let request = match plan.action {
            Action::Mapping {
                delivery_id,
                release,
                expected_previous,
            } => {
                alt_task::check_id(&delivery_id)?;
                if let Some(prior) = &expected_previous {
                    alt_task::check_id(prior)?;
                }
                if release.application != channel.application {
                    return Err(refused(
                        "release application differs from the installed channel",
                    ));
                }
                Request::Mapping(ChangeMappingRequest {
                    key,
                    channel: plan.channel,
                    configuration_digest: channel.configuration_digest,
                    delivery_id,
                    release,
                    expected_previous,
                })
            }
            Action::Withdraw {
                mapping_event_digest,
                expected_previous,
                reason,
            } => {
                alt_task::check_id(&mapping_event_digest)?;
                alt_task::check_id(&expected_previous)?;
                if reason.trim().is_empty() || reason.len() > 4096 {
                    return Err(refused("withdrawal requires a bounded reason"));
                }
                Request::Withdraw(ChangeWithdrawalRequest {
                    key,
                    channel: plan.channel,
                    configuration_digest: channel.configuration_digest,
                    mapping_event_digest,
                    expected_previous,
                    reason,
                })
            }
        };
        let saved = Saved {
            version: 1,
            binding,
            request,
        };
        let bytes = alt_json::to_vec(&saved)?;
        if bytes.len() > link::MAX_BYTES {
            return Err(refused("association intent exceeds the supported size"));
        }
        let id = alt_hash::blake3::hash(&bytes).to_hex().to_string();
        let parent = self.store.alt_dir.join("task-changes");
        std::fs::create_dir_all(&parent)?;
        crate::durable::write(parent.join(format!("{id}.json")), bytes)?;
        emit(
            out,
            json_mode,
            json!({"change_id":id,"state":"previewed","request":saved.request}),
        )
    }
    fn change_send(
        &mut self,
        id: &str,
        status: bool,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        if !status {
            self.ensure_writable("task remote changes send")?;
        }
        alt_task::check_id(id)?;
        let bytes = bounded(
            &self
                .store
                .alt_dir
                .join("task-changes")
                .join(format!("{id}.json")),
            link::MAX_BYTES,
        )?;
        if alt_hash::blake3::hash(&bytes).to_hex().as_str() != id {
            return Err(refused("saved association intent changed"));
        }
        let saved: Saved = alt_json::from_slice(&bytes)?;
        self.check_binding(&saved.binding)?;
        if saved.version != 1
            || saved.request.key().scope != saved.binding.scope
            || saved.request.key().actor_id != saved.binding.actor.actor_id
        {
            return Err(refused("saved association binding mismatch"));
        }
        saved.request.key().validate()?;
        let client = transport(&saved.binding)?;
        check_connection(&client, &saved.binding, !status)
            .map_err(|e| failed(id, saved.request.key(), e))?;
        let known = client
            .change_request(saved.request.key())
            .map_err(|e| failed(id, saved.request.key(), e))?;
        if let Some(value) = &known {
            checked(&saved, value).map_err(|e| failed(id, saved.request.key(), e))?;
        }
        if status || known.is_some() {
            return result(id, known, json_mode, out);
        }
        let pinned = key_bytes(&saved.binding.receipt_key)?;
        let value = match &saved.request {
            Request::Mapping(r) => client.change_mapping(r, &pinned),
            Request::Withdraw(r) => client.change_withdrawal(r, &pinned),
        }
        .map_err(|e| failed(id, saved.request.key(), e))?;
        checked(&saved, &value).map_err(|e| failed(id, saved.request.key(), e))?;
        result(id, Some(value), json_mode, out)
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_fixture_tests!("native-task-transfer-changes"; Saved);
    mod reads {
        use super::*;
        alt_json_testkit::module_read_tests!("native-task-transfer-changes"; Plan: Plan, Action: Action);
    }
    alt_json_testkit::module_read_tests!("state-remote"; Saved: Saved, ChangePlan: Plan);
}
