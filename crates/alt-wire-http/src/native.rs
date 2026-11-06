//! Bounded native HTTP transport. Caller owns durable intents, origin/key
//! trust, local publication previews and all decisions to resume a request.
pub mod change_links;
pub mod portable;
use alt_remote_protocol::*;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{Read, Write},
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("portable protocol: {0}")]
    Portable(#[from] alt_portable::Error),
    #[error("native protocol: {0}")]
    Protocol(#[from] alt_remote_protocol::Error),
    #[error("invalid native connection or response: {0}")]
    Invalid(&'static str),
    #[error("native transfer limit: {0}")]
    Limit(&'static str),
    #[error("native HTTP request failed ({status})")]
    Rejected {
        status: u16,
        failure: Option<Box<Failure>>,
    },
    #[error("native transport unavailable")]
    Unavailable,
    #[error("native write outcome is unknown; query the original request")]
    OutcomeUnknown { key: Box<RequestKey> },
    #[error("local transfer I/O: {0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy)]
pub struct Options {
    pub timeout: Duration,
    pub response_bytes: usize,
    pub chunk_bytes: u64,
    pub content_bytes: u64,
    /// Explicit development/operator tunnel use; only literal loopback IPs.
    pub allow_loopback_http: bool,
}
pub struct Transport {
    origin: String,
    token: String,
    agent: ureq::Agent,
    options: Options,
}
fn id(value: &str) -> Result<()> {
    Scope {
        installation_id: value.into(),
        repository_instance_id: value.into(),
        recovery_epoch: 1,
    }
    .validate()?;
    Ok(())
}
fn digest(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid("content digest"));
    }
    Ok(())
}
fn scope_path(scope: &Scope) -> Result<String> {
    scope.validate()?;
    Ok(format!(
        "/v1/repositories/{}/epochs/{}",
        scope.repository_instance_id, scope.recovery_epoch
    ))
}
fn request_path(key: &RequestKey) -> Result<String> {
    key.validate()?;
    Ok(format!(
        "{}/requests/{}/{}",
        scope_path(&key.scope)?,
        key.actor_id,
        key.request_id
    ))
}
fn root_kind(kind: RecordKind) -> &'static str {
    match kind {
        RecordKind::Task => "task",
        RecordKind::Candidate => "candidate",
        RecordKind::Evidence => "evidence",
        RecordKind::Review => "review",
        RecordKind::Integration => "integration",
        RecordKind::Delivery => "delivery",
    }
}
impl Transport {
    pub fn new(origin: &str, token: String, options: Options) -> Result<Self> {
        let url = url::Url::parse(origin).map_err(|_| Error::Invalid("server origin"))?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if url.scheme() != "https"
            && !(options.allow_loopback_http && url.scheme() == "http" && loopback)
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || options.timeout.is_zero()
            || options.response_bytes == 0
            || options.response_bytes == usize::MAX
            || options.chunk_bytes == 0
            || options.content_bytes < options.chunk_bytes
        {
            return Err(Error::Invalid("server origin or transfer bounds"));
        }
        if token.len() != 69
            || !token.starts_with("alt1_")
            || !token[5..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Invalid("credential format"));
        }
        Ok(Self {
            origin: url.origin().ascii_serialization(),
            token,
            options,
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout(options.timeout)
                .build(),
        })
    }
    pub fn issue_agent(
        &self,
        request: &delegation::DelegationRequest,
    ) -> Result<delegation::DelegationInfo> {
        request.validate()?;
        self.json("POST", "/v1/delegations", &self.encode(request)?, None)
    }
    pub fn agent_delegation(&self, credential_id: &str) -> Result<delegation::DelegationInfo> {
        digest(credential_id)?;
        self.json(
            "GET",
            &format!("/v1/delegations/{credential_id}"),
            &[],
            None,
        )
    }
    pub fn agent_delegations(&self, after: Option<&str>) -> Result<delegation::DelegationPage> {
        let path = if let Some(after) = after {
            digest(after)?;
            format!("/v1/delegations?after={after}")
        } else {
            "/v1/delegations".into()
        };
        self.json("GET", &path, &[], None)
    }
    pub fn revoke_agent(&self, credential_id: &str) -> Result<()> {
        digest(credential_id)?;
        let value: serde_json::Value = self.json(
            "POST",
            &format!("/v1/delegations/{credential_id}/revoke"),
            &[],
            None,
        )?;
        if value.get("revoked").and_then(|v| v.as_bool()) != Some(true)
            || value.get("credential_id").and_then(|v| v.as_str()) != Some(credential_id)
        {
            return Err(Error::Invalid("delegation revocation response"));
        }
        Ok(())
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    fn request(
        &self,
        method: &str,
        path: &str,
        content_type: &str,
        body: &[u8],
        key: Option<&RequestKey>,
    ) -> Result<ureq::Response> {
        let request = self
            .agent
            .request(method, &format!("{}{path}", self.origin))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept-Encoding", "identity")
            .set("Content-Type", content_type);
        let response = match request.send_bytes(body) {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(_) => {
                return Err(key.map_or(Error::Unavailable, |key| Error::OutcomeUnknown {
                    key: Box::new(key.clone()),
                }));
            }
        };
        if response.status() != 200 {
            let status = response.status();
            let failure = bounded(response, self.options.response_bytes)
                .ok()
                .and_then(|b| serde_json::from_slice::<Failure>(&b).ok())
                .map(Box::new);
            return Err(Error::Rejected { status, failure });
        }
        if response.header("Content-Encoding").is_some() {
            return Err(Error::Invalid("encoded response"));
        }
        Ok(response)
    }
    fn json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        key: Option<&RequestKey>,
    ) -> Result<T> {
        let response = self.request(method, path, "application/json", body, key)?;
        let decode = || {
            if response
                .header("Content-Type")
                .and_then(|s| s.split(';').next())
                != Some("application/json")
            {
                return Err(Error::Invalid("response media type"));
            }
            let bytes = bounded(response, self.options.response_bytes)?;
            serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("response JSON"))
        };
        decode().map_err(|error| match key {
            Some(key) => Error::OutcomeUnknown {
                key: Box::new(key.clone()),
            },
            None => error,
        })
    }
    fn encode(&self, value: &impl Serialize) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(value).map_err(|_| Error::Invalid("request JSON"))?;
        if bytes.len() > self.options.response_bytes {
            return Err(Error::Limit("request JSON bytes"));
        }
        Ok(bytes)
    }
    pub fn capabilities(&self) -> Result<CapabilityResponse> {
        let value: CapabilityResponse = self.json("GET", "/v1/capabilities", b"", None)?;
        value.capabilities.negotiate(VERSION, VERSION)?;
        digest(&value.receipt_public_key)?;
        Ok(value)
    }
    pub fn principal(&self) -> Result<PrincipalInfo> {
        let value: PrincipalInfo = self.json("GET", "/v1/principal", b"", None)?;
        id(&value.actor_id)?;
        Ok(value)
    }
    pub fn repository(&self, repository: &str) -> Result<RepositoryInfo> {
        id(repository)?;
        let value: RepositoryInfo =
            self.json("GET", &format!("/v1/repositories/{repository}"), b"", None)?;
        value.scope.validate()?;
        if value.scope.repository_instance_id != repository {
            return Err(Error::Invalid("repository response scope"));
        }
        validate_ref_name(&value.default_branch)?;
        if !value.default_branch.starts_with("refs/heads/") {
            return Err(Error::Invalid("default branch"));
        }
        for (name, oid) in &value.refs {
            validate_ref_name(name)?;
            value.object_format.validate_oid(oid)?;
        }
        Ok(value)
    }
    pub fn begin(&self, request: &BeginRequest, limits: Limits) -> Result<RequestState> {
        request
            .manifest
            .validate_publication(&request.publication, limits)?;
        let path = format!("{}/requests", scope_path(&request.publication.key.scope)?);
        let state: RequestState = self.json(
            "POST",
            &path,
            &self.encode(request)?,
            Some(&request.publication.key),
        )?;
        if state.digest() != request.publication.digest(limits)? {
            return Err(Error::Invalid("begin response intent"));
        }
        Ok(state)
    }
    pub fn query(&self, key: &RequestKey) -> Result<RequestObservation> {
        self.observation("GET", &request_path(key)?, key)
    }
    pub fn prepare(&self, key: &RequestKey) -> Result<RequestObservation> {
        self.observation("POST", &format!("{}/prepare", request_path(key)?), key)
    }
    pub fn publish(&self, key: &RequestKey) -> Result<RequestObservation> {
        self.observation("POST", &format!("{}/publish", request_path(key)?), key)
    }
    fn observation(
        &self,
        method: &str,
        path: &str,
        key: &RequestKey,
    ) -> Result<RequestObservation> {
        let value: RequestObservation =
            self.json(method, path, b"", (method != "GET").then_some(key))?;
        let returned = match &value {
            RequestObservation::Recorded { key, .. }
            | RequestObservation::Unknown { key }
            | RequestObservation::NotSeen { key } => key,
        };
        if returned != key {
            return Err(Error::Invalid("request response scope"));
        }
        Ok(value)
    }
    pub fn upload_status(&self, key: &RequestKey) -> Result<UploadStatus> {
        let value: UploadStatus =
            self.json("GET", &format!("{}/uploads", request_path(key)?), b"", None)?;
        for chunk in &value.verified_chunks {
            digest(chunk)?;
        }
        Ok(value)
    }
    pub fn upload_chunk(&self, key: &RequestKey, chunk: &Chunk, bytes: &[u8]) -> Result<()> {
        digest(&chunk.blake3)?;
        if bytes.len() as u64 != chunk.bytes
            || chunk.bytes > self.options.chunk_bytes
            || blake3::hash(bytes).to_hex().as_str() != chunk.blake3
        {
            return Err(Error::Invalid("upload chunk bytes"));
        }
        let path = format!("{}/chunks/{}", request_path(key)?, chunk.blake3);
        let response = self.request("PUT", &path, "application/octet-stream", bytes, Some(key))?;
        let value: serde_json::Value =
            serde_json::from_slice(&bounded(response, self.options.response_bytes)?)
                .map_err(|_| Error::Invalid("upload response"))?;
        if value["blake3"] != chunk.blake3 || value["verified"] != true {
            return Err(Error::Invalid("upload response identity"));
        }
        Ok(())
    }
    pub fn assemble(&self, key: &RequestKey, content: &str) -> Result<()> {
        digest(content)?;
        let value: serde_json::Value = self.json(
            "POST",
            &format!("{}/content/{content}/assemble", request_path(key)?),
            b"",
            Some(key),
        )?;
        if value["blake3"] != content || value["verified"] != true {
            return Err(Error::Invalid("assembly response identity"));
        }
        Ok(())
    }
    pub fn review(&self, key: &RequestKey) -> Result<ReviewSelection> {
        let value: ReviewSelection =
            self.json("GET", &format!("{}/review", request_path(key)?), b"", None)?;
        if &value.publication.key != key {
            return Err(Error::Invalid("review response scope"));
        }
        Ok(value)
    }
    pub fn approve(
        &self,
        key: &RequestKey,
        integration: &str,
        inspected: &ApprovalContext,
    ) -> Result<()> {
        digest(integration)?;
        let body = ApprovalRequest {
            integration_id: integration.into(),
            inspected: inspected.clone(),
        };
        let value: serde_json::Value = self.json(
            "POST",
            &format!("{}/approve", request_path(key)?),
            &self.encode(&body)?,
            Some(key),
        )?;
        if value["integration_id"] != integration
            || value["approved"] != true
            || value["request"]
                != serde_json::to_value(key).map_err(|_| Error::Invalid("request identity"))?
        {
            return Err(Error::Invalid("approval response identity"));
        }
        Ok(())
    }
    pub fn bundles(
        &self,
        scope: &Scope,
        root: &RecordRoot,
        after: Option<&str>,
    ) -> Result<Vec<PublishedBundle>> {
        root.validate()?;
        let mut path = format!(
            "{}/tasks/{}/{}",
            scope_path(scope)?,
            root_kind(root.kind),
            root.id
        );
        if let Some(after) = after {
            digest(after)?;
            path.push_str(&format!("?after={after}"));
        }
        self.json("GET", &path, b"", None)
    }
    pub fn records(
        &self,
        scope: &Scope,
        kind: RecordKind,
        after: Option<&str>,
    ) -> Result<Vec<RecordRoot>> {
        let mut path = format!("{}/tasks/{}", scope_path(scope)?, root_kind(kind));
        if let Some(after) = after {
            digest(after)?;
            path.push_str(&format!("?after={after}"));
        }
        let values: Vec<RecordRoot> = self.json("GET", &path, b"", None)?;
        for value in &values {
            value.validate()?;
            if value.kind != kind {
                return Err(Error::Invalid("record kind"));
            }
        }
        Ok(values)
    }
    /// Caller writes to a private temporary file and installs only on success.
    pub fn download(
        &self,
        scope: &Scope,
        content: &str,
        bytes: u64,
        output: &mut dyn Write,
    ) -> Result<()> {
        self.download_path(
            &format!("{}/content/{content}", scope_path(scope)?),
            content,
            bytes,
            output,
        )
    }
    pub fn review_content(
        &self,
        key: &RequestKey,
        content: &str,
        bytes: u64,
        output: &mut dyn Write,
    ) -> Result<()> {
        self.download_path(
            &format!("{}/review/content/{content}", request_path(key)?),
            content,
            bytes,
            output,
        )
    }
    fn download_path(
        &self,
        path: &str,
        content: &str,
        bytes: u64,
        output: &mut dyn Write,
    ) -> Result<()> {
        digest(content)?;
        if bytes > self.options.content_bytes {
            return Err(Error::Limit("download bytes"));
        }
        let response = self.request("GET", path, "application/octet-stream", b"", None)?;
        if response.header("Alt-Content-Blake3") != Some(content)
            || response
                .header("Content-Length")
                .and_then(|s| s.parse::<u64>().ok())
                != Some(bytes)
        {
            return Err(Error::Invalid("download identity headers"));
        }
        let mut input = response.into_reader();
        let mut hash = blake3::Hasher::new();
        let mut remaining = bytes;
        let mut buffer = [0u8; 65536];
        while remaining > 0 {
            let count = input.read(&mut buffer[..remaining.min(65536) as usize])?;
            if count == 0 {
                return Err(Error::Invalid("truncated download"));
            }
            hash.update(&buffer[..count]);
            output.write_all(&buffer[..count])?;
            remaining -= count as u64;
        }
        if input.read(&mut buffer[..1])? != 0 || hash.finalize().to_hex().as_str() != content {
            return Err(Error::Invalid("download content hash"));
        }
        Ok(())
    }
}
fn bounded(response: ureq::Response, maximum: usize) -> Result<Vec<u8>> {
    if response
        .header("Content-Length")
        .is_some_and(|s| s.parse::<u64>().ok().is_none_or(|n| n > maximum as u64))
    {
        return Err(Error::Limit("response bytes"));
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(Error::Limit("response bytes"));
    }
    Ok(bytes)
}
/// The key must come from the caller's persisted, authenticated origin binding.
/// A key included in an arbitrary downloaded receipt does not establish trust.
pub fn verify_receipt(
    receipt: &Receipt,
    publication: &Publication,
    public_key: &[u8; 32],
    limits: Limits,
) -> Result<()> {
    receipt.validate(limits)?;
    if receipt.body.request_digest != publication.digest(limits)?
        || receipt.signing_key_id != blake3::hash(public_key).to_hex().as_str()
    {
        return Err(Error::Invalid("receipt intent or signing identity"));
    }
    let signature: Vec<u8> = receipt
        .signature
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let nibble = |v: u8| if v <= b'9' { v - b'0' } else { v - b'a' + 10 };
            nibble(pair[0]) * 16 + nibble(pair[1])
        })
        .collect();
    VerifyingKey::from_bytes(public_key)
        .map_err(|_| Error::Invalid("receipt public key"))?
        .verify_strict(
            &receipt.body.signing_bytes(limits)?,
            &Signature::from_slice(&signature)
                .map_err(|_| Error::Invalid("receipt signature encoding"))?,
        )
        .map_err(|_| Error::Invalid("receipt signature"))
}
