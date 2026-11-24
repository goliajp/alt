use super::transfer_source::{LocalSource, bounded};
use super::*;
use alt_remote_protocol as protocol;
use alt_task_service::{RecordKey, TaskLimits, TaskRecord};
use alt_task_transfer::{Package, Received};
use alt_wire_http::native::{Options, Transport, verify_receipt};
use std::io::{Read, Seek, SeekFrom};
mod adoption;
mod changes;
mod command;
mod current_review;
mod publication;
mod received;
use adoption::{Adoption, Predecessor};

fn limits() -> protocol::Limits {
    protocol::Limits {
        request_bytes: 8 * 1024 * 1024,
        ref_updates: 1000,
        record_roots: 1000,
        manifest_entries: 100_000,
        graph_objects: 100_000,
        graph_edges: 1_000_000,
        structured_bytes: 4 * 1024 * 1024,
        tree_paths: 1_000_000,
        path_bytes: 4096,
        walk_steps: 10_000_000,
        chunk_bytes: 1024 * 1024,
        object_bytes: 256 * 1024 * 1024,
        total_bytes: 4 * 1024 * 1024 * 1024,
    }
}
fn task_limits() -> TaskLimits {
    TaskLimits {
        objects: 100_000,
        records: 10_000,
        edges: 100_000,
        structured_bytes: 4 * 1024 * 1024,
        total_bytes: 4 * 1024 * 1024 * 1024,
        work_bytes: 32 * 1024 * 1024 * 1024,
        artifact_bytes: 256 * 1024 * 1024,
        paths: 1_000_000,
        path_bytes: 4096,
        walk_steps: 10_000_000,
    }
}
#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct Binding {
    version: u32,
    origin: String,
    scope: protocol::Scope,
    receipt_key: String,
    actor: protocol::PrincipalInfo,
    local_repository: String,
    allow_loopback_http: bool,
    transfer_limits: protocol::Limits,
}
fn negotiated_limits(remote: protocol::Limits) -> protocol::Limits {
    let mut local = limits();
    macro_rules! bound { ($($field:ident),*)=>{$(local.$field=local.$field.min(remote.$field);)*}; }
    bound!(
        request_bytes,
        ref_updates,
        record_roots,
        manifest_entries,
        graph_objects,
        graph_edges,
        structured_bytes,
        tree_paths,
        path_bytes,
        walk_steps,
        chunk_bytes,
        object_bytes,
        total_bytes
    );
    local
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct PublishedOrigin {
    version: u32,
    binding: Binding,
    receipt: protocol::Receipt,
    #[json(default, skip_serializing_if = "Vec::is_empty")]
    provenance: Vec<protocol::TransferProof>,
}
#[derive(alt_json::FromJson)]
#[cfg_attr(test, derive(alt_json::ToJson))]
#[json(deny_unknown_fields)]
struct Plan {
    selection: alt_task_transfer::Selection,
    #[json(default)]
    refs: Vec<protocol::RefUpdate>,
}

fn key_bytes(key: &str) -> Res<[u8; 32]> {
    alt_task::check_id(key)?;
    alt_input::unhex(key)?
        .try_into()
        .map_err(|_| refused("receipt public key must be 32 bytes"))
}
fn identifier(value: &str) -> Res<()> {
    protocol::Scope {
        installation_id: value.into(),
        repository_instance_id: value.into(),
        recovery_epoch: 1,
    }
    .validate()?;
    Ok(())
}
fn kind(value: &str) -> Res<protocol::RecordKind> {
    match value {
        "task" => Ok(protocol::RecordKind::Task),
        "candidate" => Ok(protocol::RecordKind::Candidate),
        "evidence" => Ok(protocol::RecordKind::Evidence),
        "review" => Ok(protocol::RecordKind::Review),
        "integration" => Ok(protocol::RecordKind::Integration),
        "delivery" => Ok(protocol::RecordKind::Delivery),
        _ => Err(crate::machine::Failure::usage(
            "kind must be task, candidate, evidence, review, integration or delivery",
        )
        .into()),
    }
}
fn json_file<T: alt_json::FromJson>(path: &Path) -> Res<T> {
    Ok(alt_json::from_slice(&bounded(
        path,
        limits().request_bytes,
    )?)?)
}
fn write_json(path: &Path, value: &impl alt_json::ToJson) -> Res<()> {
    Ok(crate::durable::write(path, alt_json::to_vec(value)?)?)
}
fn transport(binding: &Binding) -> Res<Transport> {
    let token = if let Some(path) = std::env::var_os("ALT_SERVER_CREDENTIAL") {
        if std::env::var_os("ALT_SERVER_TOKEN").is_some() {
            return Err(refused(
                "set only one of ALT_SERVER_CREDENTIAL or ALT_SERVER_TOKEN",
            ));
        }
        crate::auth::credential_for_origin(Path::new(&path), &binding.origin)?
    } else {
        std::env::var("ALT_SERVER_TOKEN").map_err(|_| {
            refused("set ALT_SERVER_CREDENTIAL to the private file saved by alt auth login")
        })?
    };
    Ok(Transport::new(
        &binding.origin,
        token,
        Options {
            timeout: std::time::Duration::from_secs(60),
            response_bytes: limits().request_bytes,
            chunk_bytes: limits().chunk_bytes,
            content_bytes: limits().object_bytes,
            allow_loopback_http: binding.allow_loopback_http,
        },
    )?)
}
fn check_connection(client: &Transport, binding: &Binding, write: bool) -> Res<()> {
    let capability = client.capabilities()?;
    capability.capabilities.require_task_bundles()?;
    if capability.receipt_public_key != binding.receipt_key {
        return Err(refused(
            "server receipt key changed; inspect the instance before binding a new remote",
        ));
    }
    let repository = client.repository(&binding.scope.repository_instance_id)?;
    if repository.scope != binding.scope || repository.object_format != protocol::ObjectFormat::Sha1
    {
        return Err(refused(
            "repository identity or recovery epoch changed; query the original instance history",
        ));
    }
    if write {
        let principal = client.principal()?;
        if principal.actor_id != binding.actor.actor_id
            || principal.attribution != binding.actor.attribution
        {
            return Err(refused(
                "credential no longer matches the previewed actor and delegation",
            ));
        }
    }
    Ok(())
}
fn observed(
    observation: protocol::RequestObservation,
    publication: &protocol::Publication,
) -> Res<Option<protocol::RequestState>> {
    let expected = &publication.key;
    match observation {
        protocol::RequestObservation::Recorded { key, result } if &key == expected => {
            if result.digest() != publication.digest(limits())? {
                return Err(refused("server response belongs to a different intent"));
            }
            Ok(Some(*result))
        }
        protocol::RequestObservation::NotSeen { key } if &key == expected => Ok(None),
        protocol::RequestObservation::Unknown { key } if &key == expected => {
            Err(crate::machine::Failure::unknown(
                "server cannot yet confirm the original publication outcome",
            )
            .into())
        }
        _ => Err(refused("server response request identity mismatch")),
    }
}
fn remote_error(
    error: alt_wire_http::native::Error,
    key: &protocol::RequestKey,
) -> Box<dyn std::error::Error> {
    // A transport/authentication failure is not a durable publication result:
    // an earlier attempt may already have committed. Only an authenticated
    // Recorded terminal state can establish rejection/expiry.
    let mut failure = crate::machine::Failure::unknown(error.to_string());
    failure.details = Box::new(json!({"remote_request":key}));
    failure.next_action =
        "Run `alt task remote status <publication-id>` before resuming the same publication";
    failure.into()
}

impl NativeRepo<'_> {
    /// Check a specific immutable record, never just a possibly colliding
    /// request ID from another repository/worktree.
    pub(super) fn task_record_complete<T: Record>(&self, record: &T) -> Res<bool> {
        let id = record.id()?;
        let key = RecordKey {
            kind: alt_json::from_value(json!(T::KIND))?,
            id,
        };
        let raw = alt_json::to_vec(
            &json!({"schema_version":1,"hash_algorithm":"blake3","kind":T::KIND,"record":record}),
        )?;
        let decoded = TaskRecord::decode(&key, &raw, task_limits().structured_bytes)?;
        if let TaskRecord::Evidence(evidence) = &decoded
            && self.local_collected_evidence_complete(evidence)?
        {
            return Ok(true);
        }
        match super::transfer_source::completed(
            self,
            &key,
            &decoded,
            task_limits().structured_bytes,
        ) {
            Ok(_) => Ok(true),
            Err(alt_task_transfer::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                self.received_record_complete(&key)
            }
            Err(error) => Err(error.into()),
        }
    }
    pub(super) fn received_record_complete(&self, key: &RecordKey) -> Res<bool> {
        for package in self.received_packages()? {
            let received = Received::verify(&package, task_limits())?;
            if received.bundle().records.iter().any(|r| &r.record == key) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn task_adopted(&self, id: &str) -> Res<bool> {
        alt_task::check_id(id)?;
        if !self.adoption_path(id).exists() {
            return Ok(false);
        }
        let (adoption, _, _) = self.read_adoption(id)?;
        if !self.adoption_applied(&adoption)? {
            return Ok(false);
        }
        for package in self.received_packages()? {
            if package.request().publication.digest(limits())? == adoption.publication {
                let received = Received::verify(&package, task_limits())?;
                return Ok(received.bundle().records.iter().any(|r| {
                    r.record.kind == alt_task_service::RecordKind::Task && r.record.id == id
                }));
            }
        }
        Err(refused("adopted task's received publication is missing"))
    }
    fn remote_binding(&self, name: &str) -> Res<Binding> {
        identifier(name)?;
        let binding: Binding = json_file(
            &self
                .store
                .alt_dir
                .join("task-remotes")
                .join(format!("{name}.json")),
        )?;
        self.check_binding(&binding)?;
        Ok(binding)
    }
    fn check_binding(&self, binding: &Binding) -> Res<()> {
        binding.scope.validate()?;
        binding.transfer_limits.validate()?;
        if negotiated_limits(binding.transfer_limits) != binding.transfer_limits {
            return Err(refused("saved transfer limits exceed client bounds"));
        }
        key_bytes(&binding.receipt_key)?;
        if binding.version != 1 || binding.local_repository != self.task_scope()?.repository {
            return Err(refused(
                "server binding belongs to another local repository",
            ));
        }
        Ok(())
    }
    pub(super) fn received_packages(&self) -> Res<Vec<Package>> {
        let directory = self.store.alt_dir.join("received-tasks");
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                return Err(refused("invalid received history directory"));
            }
            if !entry.path().join("origin.json").exists() {
                continue;
            }
            paths.push(entry.path());
            if paths.len() > 1024 {
                return Err(refused(
                    "received publication inventory exceeds client limit",
                ));
            }
        }
        paths.sort();
        let mut packages = Vec::new();
        for path in paths {
            let origin: PublishedOrigin = json_file(&path.join("origin.json"))?;
            self.check_binding(&origin.binding)?;
            let package = Package::open(&path.join("sealed"), limits())?;
            let publication = &package.request().publication;
            if origin.version != if origin.provenance.is_empty() { 1 } else { 2 }
                || path.file_name().and_then(|n| n.to_str())
                    != Some(publication.digest(limits())?.as_str())
            {
                return Err(refused("received publication origin mismatch"));
            }
            alt_provenance::verify_origin(
                publication,
                &origin.receipt,
                &origin.provenance,
                &origin.binding.scope,
                &key_bytes(&origin.binding.receipt_key)?,
                limits(),
            )?;
            packages.push(package);
        }
        Ok(packages)
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;
    alt_json_testkit::module_fixture_tests!("native-task-transfer"; Binding, PublishedOrigin);
}
