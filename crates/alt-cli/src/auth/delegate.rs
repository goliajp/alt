use super::*;
use alt_remote_protocol::{
    ActorKind, Attribution,
    delegation::{AgentScope, DelegationInfo, DelegationRequest, MAX_LIFETIME_MS},
};

#[derive(alt_json::FromJson)]
#[cfg_attr(test, derive(alt_json::ToJson))]
#[json(deny_unknown_fields)]
struct Plan {
    purpose: String,
    agent_session: String,
    lifetime_seconds: u64,
    scope: AgentScope,
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct PendingAgent {
    version: u32,
    origin: String,
    installation_id: String,
    receipt_public_key: String,
    parent_digest: String,
    request: DelegationRequest,
}
pub(super) fn unknown(message: &str) -> Box<dyn std::error::Error> {
    let mut failure = Failure::unknown(message);
    failure.next_action = "Resume auth delegate with the same agent file, or inspect/revoke its credential ID with auth delegations / auth revoke-agent";
    failure.into()
}
fn lock(directory: &Path) -> Res<File> {
    let meta = std::fs::symlink_metadata(directory)?;
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != effective_uid() {
        return Err(refused(
            "delegation journal must be an owned private directory",
        ));
    }
    let path = directory.join("lock");
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file.sync_all()?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let file = private_file(&path, true)?;
    file.lock()?;
    Ok(file)
}
fn clean(directory: &Path) -> bool {
    for name in ["request.json", "lock"] {
        if let Err(e) = std::fs::remove_file(directory.join(name))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return false;
        }
    }
    std::fs::remove_dir(directory)
        .and_then(|()| File::open(parent(directory))?.sync_all())
        .is_ok()
}
fn matches(info: &DelegationInfo, req: &DelegationRequest) -> Res<()> {
    if info.credential_id != alt_remote_protocol::delegation::credential_id(&req.access_token)?
        || info.attribution != req.attribution
        || info.purpose != req.purpose
        || info.expires_at_unix_ms != req.expires_at_unix_ms
        || alt_json::to_value(&info.scope)? != alt_json::to_value(&req.scope)?
        || info.revoked
    {
        return Err(refused(
            "delegation result differs from the saved request or is revoked",
        ));
    }
    Ok(())
}
pub(super) fn run(
    parent_path: &Path,
    path: &Path,
    plan: Option<&Path>,
    resume: bool,
) -> Res<alt_json::Value> {
    let human = read_credential(parent_path)?;
    if human.principal.attribution.kind != ActorKind::Human {
        return Err(refused("delegation requires a current human credential"));
    }
    if path.try_exists()? && !resume {
        return Err(refused(
            "agent destination already exists; choose another file or use --resume",
        ));
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(".delegation");
    let directory = PathBuf::from(name);
    if !directory.try_exists()? && path.try_exists()? && resume {
        let child = read_credential(path)?;
        if child.origin != human.origin
            || child.installation_id != human.installation_id
            || child.receipt_public_key != human.receipt_public_key
            || child.principal.attribution.kind != ActorKind::Agent
            || child.principal.attribution.on_behalf_of.as_deref()
                != Some(&human.principal.actor_id)
        {
            return Err(refused(
                "agent destination belongs to another origin, installation or owner",
            ));
        }
        inspect(&child)?;
        return Ok(summary(&child, path));
    }
    if directory.try_exists()? {
        if !resume {
            return Err(refused(
                "pending delegation exists; resume with the same agent file",
            ));
        }
    } else {
        if resume {
            return Err(refused(
                "no saved delegation exists; no new request was started",
            ));
        }
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        File::open(parent(&directory))?.sync_all()?;
    }
    let _lock = lock(&directory)?;
    let request_path = directory.join("request.json");
    if !request_path.try_exists()? {
        if resume {
            return Err(refused(
                "delegation journal has no durable request; no new request was started",
            ));
        }
        let plan = plan.ok_or_else(|| refused("delegation plan required"))?;
        let plan: Plan = read(&mut File::open(plan)?)?;
        if plan.lifetime_seconds == 0
            || plan.lifetime_seconds > MAX_LIFETIME_MS / 1000
            || plan.scope.repository.installation_id != human.installation_id
        {
            return Err(refused("invalid delegation lifetime or installation"));
        }
        let expires = now()?.saturating_add(plan.lifetime_seconds * 1000);
        if expires > human.expires_at_unix_ms {
            return Err(refused("delegation would outlive the parent credential"));
        }
        let mut entropy = [0u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut entropy)?;
        let secret: String = entropy.iter().map(|b| format!("{b:02x}")).collect();
        let request = DelegationRequest {
            version: 1,
            access_token: format!("alt1_{secret}"),
            attribution: Attribution {
                kind: ActorKind::Agent,
                on_behalf_of: Some(human.principal.actor_id.clone()),
                delegation_id: Some(alt_hash::blake3::hash(&entropy).to_hex().to_string()),
                agent_session: Some(plan.agent_session),
            },
            expires_at_unix_ms: expires,
            purpose: plan.purpose,
            scope: plan.scope,
        };
        request.validate()?;
        save(
            &request_path,
            &PendingAgent {
                version: 1,
                origin: human.origin.clone(),
                installation_id: human.installation_id.clone(),
                receipt_public_key: human.receipt_public_key.clone(),
                parent_digest: alt_hash::blake3::hash(human.access_token.as_bytes())
                    .to_hex()
                    .to_string(),
                request,
            },
        )?;
    }
    let pending: PendingAgent = read(&mut private_file(&request_path, false)?)?;
    pending.request.validate()?;
    if pending.version != 1
        || pending.origin != human.origin
        || pending.installation_id != human.installation_id
        || pending.receipt_public_key != human.receipt_public_key
        || pending.parent_digest
            != alt_hash::blake3::hash(human.access_token.as_bytes())
                .to_hex()
                .as_str()
        || pending.request.attribution.on_behalf_of.as_deref() != Some(&human.principal.actor_id)
    {
        return Err(refused(
            "saved delegation belongs to another parent credential or installation",
        ));
    }
    let child = SavedCredential {
        version: 1,
        origin: human.origin.clone(),
        installation_id: human.installation_id.clone(),
        receipt_public_key: human.receipt_public_key.clone(),
        principal: alt_remote_protocol::PrincipalInfo {
            actor_id: human.principal.actor_id.clone(),
            attribution: pending.request.attribution.clone(),
        },
        access_token: pending.request.access_token.clone(),
        expires_at_unix_ms: pending.request.expires_at_unix_ms,
        allow_loopback_http: human.allow_loopback_http,
    };
    child.validate()?;
    inspect(&human)?;
    if path.try_exists()? {
        let saved = read_credential(path)?;
        if alt_json::to_value(&saved)? != alt_json::to_value(&child)? {
            return Err(refused(
                "agent destination differs from the saved delegation",
            ));
        }
        inspect(&child)?;
    } else {
        let client = transport(&human)?;
        let info = client
            .issue_agent(&pending.request)
            .map_err(|error| match error {
                alt_wire_http::native::Error::Rejected {
                    status: 400 | 401 | 403 | 409 | 413,
                    ..
                } => refused("agent authorization refused; saved intent retained for inspection"),
                _ => {
                    unknown("delegation result is unconfirmed; resume the same saved agent request")
                }
            })?;
        matches(&info, &pending.request)?;
        inspect(&child).map_err(|_|unknown("agent was issued; identity verification is unconfirmed; resume the same saved request"))?;
        save(path, &child).map_err(|_| {
            unknown("agent file save is unconfirmed; resume the same saved request")
        })?;
    }
    let mut result = summary(&child, path);
    result["credential_id"] = json!(alt_remote_protocol::delegation::credential_id(
        &child.access_token
    )?);
    result["scope"] = alt_json::to_value(&pending.request.scope)?;
    result["purpose"] = json!(pending.request.purpose);
    result["pending_state_removed"] = json!(clean(&directory));
    Ok(result)
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_fixture_tests!("auth-delegate"; PendingAgent);
    alt_json_testkit::module_read_tests!("auth-delegate"; Plan: Plan);
}
