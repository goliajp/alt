//! Existing whole-command receipt encoding, independent of filesystem and CLI.
//! These are source facts; reading a receipt does not authorize replay or make
//! its original command an execution performed by the importing instance.
use alt_json::{FromJson, ToJson};
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Debug, Serialize, Deserialize, ToJson, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct Intent {
    pub version: u32,
    pub hash_algorithm: String,
    pub request_id: String,
    pub binding_digest: String,
    pub repository: String,
    pub workspace: String,
    pub started_ms: u128,
}
#[derive(Clone, Debug, Serialize, Deserialize, ToJson, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct Completion {
    pub version: u32,
    pub hash_algorithm: String,
    pub request_id: String,
    pub binding_digest: String,
    pub finished_ms: u128,
    pub response_digest: String,
    pub response_bytes: u64,
    pub operations: Vec<String>,
    pub outcome: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub exit_code: u8,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
impl Response {
    /// Existing v1 daemon representation: exit byte, little-endian u32 stdout
    /// length/payload, then little-endian u32 stderr length/payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(self.exit_code);
        for stream in [&self.stdout, &self.stderr] {
            bytes.extend_from_slice(&(stream.len() as u32).to_le_bytes());
            bytes.extend_from_slice(stream);
        }
        bytes
    }
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let Some(exit) = bytes.first() else {
            return Err(invalid());
        };
        let mut position = 1usize;
        let mut take = || -> io::Result<Vec<u8>> {
            let header_end = position
                .checked_add(4)
                .filter(|p| *p <= bytes.len())
                .ok_or_else(invalid)?;
            let length = u32::from_le_bytes(
                bytes[position..header_end]
                    .try_into()
                    .map_err(|_| invalid())?,
            ) as usize;
            let end = header_end
                .checked_add(length)
                .filter(|p| *p <= bytes.len())
                .ok_or_else(invalid)?;
            let stream = bytes[header_end..end].to_vec();
            position = end;
            Ok(stream)
        };
        let stdout = take()?;
        let stderr = take()?;
        if position != bytes.len() {
            return Err(invalid());
        }
        Ok(Self {
            exit_code: *exit,
            stdout,
            stderr,
        })
    }
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid command response framing",
    )
}
#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Response(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(v) => write!(f, "invalid command receipt: {v}"),
            Self::Response(e) => write!(f, "command response: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Response(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Response(e)
    }
}
pub fn validate_completion(
    intent: &Intent,
    completion: &Completion,
    response: &[u8],
) -> Result<Response, Error> {
    if intent.version != 1
        || intent.hash_algorithm != "blake3"
        || completion.version != 1
        || completion.hash_algorithm != "blake3"
        || completion.request_id != intent.request_id
        || completion.binding_digest != intent.binding_digest
        || !matches!(
            completion.outcome.as_str(),
            "applied" | "not_applied" | "partial" | "unknown"
        )
    {
        return Err(Error::Invalid("intent/completion binding or encoding"));
    }
    if completion.response_bytes != response.len() as u64
        || completion.response_digest != alt_hash::blake3::hash(response).to_hex().as_str()
    {
        return Err(Error::Invalid("response content identity"));
    }
    Ok(Response::decode(response)?)
}
