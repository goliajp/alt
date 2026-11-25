//! The `alt task remote` commands, each sent to what carries it out.
use super::*;

impl NativeRepo<'_> {
    pub(in crate::native::task) fn task_remote_command(
        &mut self,
        op: &crate::cli::TaskRemoteOp,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use crate::cli::TaskRemoteOp;
        match op {
            TaskRemoteOp::Changes { op } => self.change_command(op, json_mode, out),
            TaskRemoteOp::Connect {
                name,
                url,
                repository,
                receipt_key,
                allow_loopback_http,
            } => {
                self.ensure_writable("task remote connect")?;
                identifier(name)?;
                identifier(repository)?;
                key_bytes(receipt_key)?;
                let directory = self.store.alt_dir.join("task-remotes");
                std::fs::create_dir_all(&directory)?;
                let path = directory.join(format!("{name}.json"));
                if path.exists() {
                    return Err(refused(
                        "remote binding already exists; use a new explicit name for another identity",
                    ));
                }
                let mut binding = Binding {
                    version: 1,
                    origin: url.clone(),
                    scope: protocol::Scope {
                        installation_id: "pending".into(),
                        repository_instance_id: repository.clone(),
                        recovery_epoch: 1,
                    },
                    receipt_key: receipt_key.clone(),
                    actor: protocol::PrincipalInfo {
                        actor_id: "pending".into(),
                        attribution: protocol::Attribution {
                            kind: protocol::ActorKind::Human,
                            on_behalf_of: None,
                            delegation_id: None,
                            agent_session: None,
                        },
                    },
                    local_repository: self.task_scope()?.repository,
                    allow_loopback_http: *allow_loopback_http,
                    transfer_limits: limits(),
                };
                let client = transport(&binding)?;
                let caps = client.capabilities()?;
                caps.capabilities.require_task_bundles()?;
                if caps.receipt_public_key != *receipt_key {
                    return Err(refused(
                        "server receipt key does not match the supplied trusted key",
                    ));
                }
                binding.transfer_limits = negotiated_limits(caps.capabilities.limits);
                let info = client.repository(repository)?;
                if info.object_format != protocol::ObjectFormat::Sha1 {
                    return Err(refused("unsupported remote object format"));
                }
                binding.scope = info.scope;
                binding.actor = client.principal()?;
                binding.origin = client.origin().into();
                write_json(&path, &binding)?;
                emit(
                    out,
                    json_mode,
                    json!({"remote":name,"origin":binding.origin,"scope":binding.scope,"actor":binding.actor,"receipt_key":binding.receipt_key}),
                )
            }
            TaskRemoteOp::Preview { remote, plan } => {
                self.publication_preview(remote, plan, json_mode, out)
            }
            TaskRemoteOp::Publish { id, prepare_only } => {
                self.publication_send(id, false, *prepare_only, json_mode, out)
            }
            TaskRemoteOp::Status { id } => self.publication_send(id, true, false, json_mode, out),
            TaskRemoteOp::Attest { evidence, key_file } => {
                self.collector_attest(evidence, key_file, json_mode, out)
            }
            TaskRemoteOp::Review {
                remote,
                actor,
                request,
            } => self.remote_review(remote, actor, request, json_mode, out),
            TaskRemoteOp::Approve {
                remote,
                actor,
                request,
                integration,
                digest,
            } => self.remote_approve(remote, actor, request, integration, digest, json_mode, out),
            TaskRemoteOp::List {
                remote,
                kind: record_kind,
                id,
                after,
            } => {
                let binding = self.remote_binding(remote)?;
                let client = transport(&binding)?;
                check_connection(&client, &binding, false)?;
                let kind = kind(record_kind)?;
                let value = if let Some(id) = id {
                    alt_json::to_value(client.bundles(
                        &binding.scope,
                        &protocol::RecordRoot {
                            kind,
                            id: id.clone(),
                        },
                        after.as_deref(),
                    )?)?
                } else {
                    alt_json::to_value(client.records(&binding.scope, kind, after.as_deref())?)?
                };
                emit(
                    out,
                    json_mode,
                    json!({"remote":remote,"scope":binding.scope,"items":value}),
                )
            }
            TaskRemoteOp::Receive {
                remote,
                kind: record_kind,
                id,
                publication,
            } => self.publication_receive(
                remote,
                protocol::RecordRoot {
                    kind: kind(record_kind)?,
                    id: id.clone(),
                },
                publication,
                json_mode,
                out,
            ),
            TaskRemoteOp::Adopt {
                task,
                publication,
                reconcile_request,
            } => self.received_adopt(
                task,
                publication,
                reconcile_request.as_deref(),
                json_mode,
                out,
            ),
        }
    }
}
