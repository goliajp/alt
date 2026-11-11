//! Device login has its own private journal, outside repository operation logs.
//! Secrets are saved before instructions are shown and before success is printed.
mod delegate;
use crate::{cli::AuthOp, machine::Failure};
use alt_json::json;
use alt_remote_protocol::login::{DeviceAuthorizationResponse, DeviceError, SavedCredential};
use alt_wire_http::{
    device::{Client, Error as DeviceTransportError},
    native::{Options, Transport},
};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Res<T> = Result<T, Box<dyn std::error::Error>>;
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct Pending {
    version: u32,
    origin: String,
    allow_loopback_http: bool,
    started_at_unix_ms: u64,
    authorization: DeviceAuthorizationResponse,
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct Issued {
    version: u32,
    device_digest: String,
    access_token: String,
    expires_at_unix_ms: u64,
}
fn effective_uid() -> u32 {
    alt_os::sys::geteuid()
}
fn unconfirmed(message: &str) -> Box<dyn std::error::Error> {
    let mut failure = Failure::unknown(message);
    failure.next_action = "Resume the same auth login journal, or retry auth logout with the retained credential file";
    failure.into()
}
fn now() -> Res<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}
fn refused(message: &str) -> Box<dyn std::error::Error> {
    Failure::refusal("authorization_refused", message).into()
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}
fn private_file(path: &Path, write: bool) -> Res<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    // Credentials are never accepted through a symlink or a shared file.
    if !meta.is_file()
        || meta.mode() & 0o077 != 0
        || meta.uid() != effective_uid()
        || meta.len() > 16_384
    {
        return Err(refused(
            "credential state must be an owned private regular file",
        ));
    }
    Ok(file)
}
fn read<T: alt_json::FromJson>(file: &mut File) -> Res<T> {
    let mut bytes = Vec::new();
    file.take(16_385).read_to_end(&mut bytes)?;
    if bytes.len() > 16_384 {
        return Err(refused("credential state is too large"));
    }
    Ok(alt_json::from_slice(&bytes)?)
}
fn save(path: &Path, value: &impl alt_json::ToJson) -> Res<()> {
    let mut temp = alt_os::temp::NamedTempFile::new_in(parent(path))?;
    temp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    alt_json::to_writer(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)?;
    File::open(parent(path))?.sync_all()?;
    Ok(())
}
pub fn read_credential(path: &Path) -> Res<SavedCredential> {
    let value: SavedCredential = read(&mut private_file(path, false)?)?;
    value.validate()?;
    if Client::new(&value.origin, value.allow_loopback_http)?.origin() != value.origin {
        return Err(refused("credential origin is not canonical"));
    }
    Ok(value)
}
pub fn credential_for_origin(path: &Path, origin: &str) -> Res<String> {
    let value = read_credential(path)?;
    if value.origin != origin {
        return Err(refused("credential belongs to a different server origin"));
    }
    Ok(value.access_token)
}
fn transport(value: &SavedCredential) -> Res<Transport> {
    Ok(Transport::new(
        &value.origin,
        value.access_token.clone(),
        Options {
            timeout: Duration::from_secs(10),
            response_bytes: 1_048_576,
            chunk_bytes: 1_048_576,
            content_bytes: 1_048_576,
            allow_loopback_http: value.allow_loopback_http,
        },
    )?)
}
fn inspect(value: &SavedCredential) -> Res<()> {
    let client = transport(value)?;
    let capability = client.capabilities()?;
    let principal = client.principal()?;
    if capability.capabilities.installation_id != value.installation_id
        || capability.receipt_public_key != value.receipt_public_key
        || principal.actor_id != value.principal.actor_id
        || principal.attribution != value.principal.attribution
    {
        return Err(refused(
            "server installation or principal differs from the saved credential",
        ));
    }
    Ok(())
}
fn summary(value: &SavedCredential, path: &Path) -> alt_json::Value {
    json!({"schema_version":1,"authenticated":true,"origin":value.origin,"installation_id":value.installation_id,"principal":value.principal,"expires_at_unix_ms":value.expires_at_unix_ms.to_string(),"credential_file":path})
}
fn journal(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".login");
    PathBuf::from(name)
}
fn clean_journal(path: &Path) -> bool {
    for name in ["request.json", "issued.json", "lock"] {
        if let Err(error) = std::fs::remove_file(path.join(name))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return false;
        }
    }
    std::fs::remove_dir(path)
        .and_then(|()| File::open(parent(path))?.sync_all())
        .is_ok()
}
fn clean_completed_journal(path: &Path, value: &SavedCredential) -> Res<bool> {
    let directory = journal(path);
    if !directory.try_exists()? {
        return Ok(true);
    }
    let meta = std::fs::symlink_metadata(&directory)?;
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != effective_uid() {
        return Ok(false);
    }
    let lock = private_file(&directory.join("lock"), true)?;
    lock.lock()?;
    let pending: Pending = read(&mut private_file(&directory.join("request.json"), false)?)?;
    let issued: Issued = read(&mut private_file(&directory.join("issued.json"), false)?)?;
    if pending.version != 1
        || issued.version != 1
        || pending.origin != value.origin
        || pending.allow_loopback_http != value.allow_loopback_http
        || issued.access_token != value.access_token
        || issued.expires_at_unix_ms != value.expires_at_unix_ms
        || issued.device_digest
            != alt_hash::blake3::hash(pending.authorization.device_code.as_bytes())
                .to_hex()
                .as_str()
    {
        return Ok(false);
    }
    Ok(clean_journal(&directory))
}
fn remove_opened(path: &Path, file: &File) -> Res<()> {
    let expected = file.metadata()?;
    let current = std::fs::symlink_metadata(path)?;
    if expected.dev() != current.dev() || expected.ino() != current.ino() {
        return Err(refused(
            "credential file changed while the request was running",
        ));
    }
    std::fs::remove_file(path)?;
    File::open(parent(path))?.sync_all()?;
    Ok(())
}
pub fn run(op: &AuthOp, out: &mut impl Write, progress: &mut impl Write) -> Res<()> {
    let result = match op {
        AuthOp::Delegate {
            credential_file,
            agent_file,
            plan,
            resume,
        } => delegate::run(credential_file, agent_file, plan.as_deref(), *resume)?,
        AuthOp::Delegations {
            credential_file,
            after,
        } => {
            let value = read_credential(credential_file)?;
            inspect(&value)?;
            alt_json::to_value(transport(&value)?.agent_delegations(after.as_deref())?)?
        }
        AuthOp::RevokeAgent {
            credential_file,
            credential_id,
        } => {
            let value = read_credential(credential_file)?;
            inspect(&value)?;
            transport(&value)?.revoke_agent(credential_id).map_err(|_|delegate::unknown("agent revocation is unconfirmed; list the same credential ID or retry revocation"))?;
            json!({"schema_version":1,"revoked":true,"credential_id":credential_id})
        }
        AuthOp::Login {
            server,
            credential_file,
            allow_loopback_http,
            resume,
            no_wait,
        } => login(
            server,
            credential_file,
            *allow_loopback_http,
            *resume,
            *no_wait,
            progress,
        )?,
        AuthOp::Status { credential_file } => {
            let value = read_credential(credential_file)?;
            inspect(&value)?;
            summary(&value, credential_file)
        }
        AuthOp::Logout { credential_file } => {
            let mut file = private_file(credential_file, true)?;
            file.lock()?;
            let value: SavedCredential = read(&mut file)?;
            value.validate()?;
            let client = Client::new(&value.origin, value.allow_loopback_http)?;
            if client.origin() != value.origin {
                return Err(refused("credential origin is not canonical"));
            }
            client.revoke(&value.access_token).map_err(|_| {
                unconfirmed("revocation is unconfirmed; keep the credential file and retry logout")
            })?;
            remove_opened(credential_file,&file).map_err(|_|unconfirmed("server revocation is confirmed, but local credential-file removal is unconfirmed; retry logout"))?;
            json!({"schema_version":1,"revoked":true,"origin":value.origin,"credential_file_removed":credential_file})
        }
    };
    alt_json::to_writer(&mut *out, &result)?;
    writeln!(out)?;
    Ok(())
}
fn login(
    server: &str,
    path: &Path,
    allow_http: bool,
    resume: bool,
    no_wait: bool,
    progress: &mut impl Write,
) -> Res<alt_json::Value> {
    let client = Client::new(server, allow_http)?;
    if path.try_exists()? {
        if !resume {
            return Err(refused(
                "credential destination already exists; use status or resume, or choose another file",
            ));
        }
        let value = read_credential(path)?;
        if value.origin != client.origin() {
            return Err(refused("credential belongs to another origin"));
        }
        inspect(&value)?;
        let mut result = summary(&value, path);
        result["pending_state_removed"] =
            json!(clean_completed_journal(path, &value).unwrap_or(false));
        return Ok(result);
    }
    let directory = journal(path);
    if directory.try_exists()? {
        if !resume {
            return Err(refused(
                "pending login exists; use --resume with the same credential destination",
            ));
        }
    } else {
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        File::open(parent(&directory))?.sync_all()?;
    }
    let meta = std::fs::symlink_metadata(&directory)?;
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != effective_uid() {
        return Err(refused("login journal must be an owned private directory"));
    }
    let lock_path = directory.join("lock");
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock_path)
    {
        Ok(file) => file.sync_all()?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let lock = private_file(&lock_path, true)?;
    lock.lock()?;
    // A concurrent resume may have completed while this process waited for the
    // original journal lock. Never start another authorization in that case.
    if path.try_exists()? {
        let value = read_credential(path)?;
        if value.origin != client.origin() {
            return Err(refused("credential belongs to another origin"));
        }
        inspect(&value)?;
        return Ok(summary(&value, path));
    }
    let request_path = directory.join("request.json");
    if !request_path.try_exists()? {
        if directory.join("issued.json").try_exists()? {
            return Err(refused("issued credential has no matching login request"));
        }
        let started = now()?;
        let authorization = client.begin()?;
        save(
            &request_path,
            &Pending {
                version: 1,
                origin: client.origin().into(),
                allow_loopback_http: allow_http,
                started_at_unix_ms: started,
                authorization,
            },
        )?;
    }
    let pending: Pending = read(&mut private_file(&request_path, false)?)?;
    if pending.version != 1
        || pending.origin != client.origin()
        || pending.allow_loopback_http != allow_http
    {
        return Err(refused(
            "pending login belongs to another origin or transport policy",
        ));
    }
    client.validate_authorization(&pending.authorization)?;
    let issued_path = directory.join("issued.json");
    if !issued_path.try_exists()? {
        let time = now()?;
        let end = pending
            .started_at_unix_ms
            .saturating_add(pending.authorization.expires_in.saturating_mul(1000));
        if time < pending.started_at_unix_ms || time >= end {
            return Err(refused(
                "device request expired; no new request was started. Inspect terminal credentials in Settings before starting another login",
            ));
        }
        writeln!(
            progress,
            "Open {}\nConfirm terminal code: {}",
            pending.authorization.verification_uri_complete, pending.authorization.user_code
        )?;
        progress.flush()?;
        if no_wait {
            return Ok(
                json!({"schema_version":1,"status":"authorization_pending","verification_uri":pending.authorization.verification_uri,"verification_uri_complete":pending.authorization.verification_uri_complete,"user_code":pending.authorization.user_code,"credential_file":path,"resume":true}),
            );
        }
        let deadline = Instant::now() + Duration::from_millis(end - time);
        let mut interval = Duration::from_secs(pending.authorization.interval);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining <= interval {
                return Err(refused(
                    "device request expired; pending state was retained",
                ));
            }
            std::thread::sleep(interval);
            let started = now()?;
            match client.poll(&pending.authorization.device_code) {
                Ok(value) => {
                    save(
                        &issued_path,
                        &Issued {
                            version: 1,
                            device_digest: alt_hash::blake3::hash(
                                pending.authorization.device_code.as_bytes(),
                            )
                            .to_hex()
                            .to_string(),
                            access_token: value.access_token,
                            expires_at_unix_ms: started.saturating_add(value.expires_in * 1000),
                        },
                    )
                    .map_err(|_| {
                        unconfirmed(
                            "issued credential save is unconfirmed; resume the same login journal",
                        )
                    })?;
                    break;
                }
                Err(DeviceTransportError::Rejected(DeviceError::AuthorizationPending)) => (),
                Err(DeviceTransportError::Rejected(DeviceError::SlowDown)) => {
                    interval = interval.saturating_add(Duration::from_secs(5))
                }
                Err(
                    DeviceTransportError::Unavailable
                    | DeviceTransportError::Rejected(DeviceError::TemporarilyUnavailable),
                ) => interval = interval.saturating_mul(2).min(Duration::from_secs(60)),
                Err(error) => return Err(error.into()),
            }
        }
    }
    let issued: Issued = read(&mut private_file(&issued_path, false)?)?;
    if issued.version != 1
        || issued.device_digest
            != alt_hash::blake3::hash(pending.authorization.device_code.as_bytes())
                .to_hex()
                .as_str()
        || issued.expires_at_unix_ms <= now()?
    {
        return Err(refused(
            "saved issued credential does not match this login or has expired",
        ));
    }
    let mut value = SavedCredential {
        version: 1,
        origin: client.origin().into(),
        installation_id: "pending".into(),
        receipt_public_key: "0".repeat(64),
        principal: alt_remote_protocol::PrincipalInfo {
            actor_id: "pending".into(),
            attribution: alt_remote_protocol::Attribution {
                kind: alt_remote_protocol::ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
        },
        access_token: issued.access_token,
        expires_at_unix_ms: issued.expires_at_unix_ms,
        allow_loopback_http: allow_http,
    };
    let native = transport(&value)?;
    let capability=native.capabilities().map_err(|_|unconfirmed("authorization was saved; installation verification is unconfirmed; resume the same login journal"))?;
    value.principal=native.principal().map_err(|_|unconfirmed("authorization was saved; principal verification is unconfirmed; resume the same login journal"))?;
    value.installation_id = capability.capabilities.installation_id;
    value.receipt_public_key = capability.receipt_public_key;
    value.validate()?;
    if path.try_exists()? {
        let existing = read_credential(path)?;
        if existing.access_token != value.access_token
            || existing.origin != value.origin
            || existing.installation_id != value.installation_id
            || existing.receipt_public_key != value.receipt_public_key
        {
            return Err(refused(
                "credential destination changed; pending authorization was retained",
            ));
        }
    } else {
        save(path, &value).map_err(|_| {
            unconfirmed("credential-file save is unconfirmed; resume the same login journal")
        })?;
    }
    let mut result = summary(&value, path);
    result["pending_state_removed"] = json!(clean_journal(&directory));
    Ok(result)
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_fixture_tests!("auth"; Pending, Issued);
    alt_json_testkit::module_read_tests!("state-auth"; Pending: Pending, Issued: Issued, SavedCredential: SavedCredential);
}
