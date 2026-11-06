//! Bounded cross-product transport, independent from product DBs and accounts.
//! All destinations are operator provisioned. Signed event bytes cannot select
//! a URL or a proxy. Retrying requires the caller's original immutable event.
use golia_change_link::{MAX_BYTES, Receipt, SignedEvent, transport::Target};
use std::time::Duration;

pub const INBOX_PATH: &str = "/api/v1/change-links/inbox";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Configuration,
    Dns,
    AddressPolicy,
    Transport,
    Http(u16),
    Response,
}
#[derive(Debug, thiserror::Error)]
#[error("change-link transport {code:?}; result may be committed: {may_have_sent}")]
pub struct Failure {
    pub code: Code,
    pub may_have_sent: bool,
}
fn fail(code: Code, may_have_sent: bool) -> Failure {
    Failure {
        code,
        may_have_sent,
    }
}

pub async fn send(
    target: &Target,
    credential: &str,
    event: &SignedEvent,
    receiver_key: &ed25519_dalek::VerifyingKey,
) -> Result<Receipt, Failure> {
    // Reject malformed headers before resolution/connection; no error includes
    // credentials, query strings, peer response text or database details.
    if credential.is_empty()
        || credential.len() > 8192
        || !credential.bytes().all(|b| (0x21..=0x7e).contains(&b))
    {
        return Err(fail(Code::Configuration, false));
    }
    event
        .digest()
        .map_err(|_| fail(Code::Configuration, false))?;
    let body = serde_json::to_vec(event).map_err(|_| fail(Code::Configuration, false))?;
    if body.len() > MAX_BYTES {
        return Err(fail(Code::Configuration, false));
    }
    let origin = target.url().map_err(|_| fail(Code::Configuration, false))?;
    let host = origin
        .host_str()
        .ok_or(fail(Code::Configuration, false))?
        .trim_matches(['[', ']']);
    let port = origin
        .port_or_known_default()
        .ok_or(fail(Code::Configuration, false))?;
    let resolved = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| fail(Code::Dns, false))?
    .map_err(|_| fail(Code::Dns, false))?
    .take(33)
    .collect::<Vec<_>>();
    let addresses = target
        .resolved(&resolved)
        .map_err(|_| fail(Code::AddressPolicy, false))?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .resolve_to_addrs(host, &addresses)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .user_agent("golia-change-link/1")
        .build()
        .map_err(|_| fail(Code::Configuration, false))?;
    let url = origin
        .join(INBOX_PATH)
        .map_err(|_| fail(Code::Configuration, false))?;
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut response = client
            .post(url)
            .bearer_auth(credential)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| fail(Code::Transport, true))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(fail(Code::Http(response.status().as_u16()), true));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
        {
            return Err(fail(Code::Response, true));
        }
        let mut raw = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| fail(Code::Transport, true))?
        {
            if chunk.len() > MAX_BYTES - raw.len() {
                return Err(fail(Code::Response, true));
            }
            raw.extend_from_slice(&chunk);
        }
        let receipt: Receipt =
            serde_json::from_slice(&raw).map_err(|_| fail(Code::Response, true))?;
        receipt
            .verify(event, receiver_key)
            .map_err(|_| fail(Code::Response, true))?;
        Ok(receipt)
    })
    .await
    .map_err(|_| fail(Code::Transport, true))?
}
