//! Endpoint handlers — pure logic over a [`Source`], returns owned bytes
//! plus a status code so [`router`](crate::router) doesn't need to know
//! anything about JSON.
//!
//! All payloads are hand-written JSON: the project does not pull
//! `serde_json` for a handful of stable shapes, and the static-marketing
//! API surface is small enough that escape logic stays local. Each
//! handler is independently unit-testable without booting a server.

use alt_repo::Repository;

use crate::{ApiError, Source};

/// `GET /api/version` — fixed compile-time identifiers for the build.
///
/// Stable shape: `{"schema_version": 1, "version": "...", "build": "..."}`.
/// `version` mirrors `CARGO_PKG_VERSION`; `build` is a free-form tag the
/// deploy can override via `ALT_WEB_BUILD` at runtime, falling back to
/// the literal `"dev"`.
pub fn handle_version() -> (u16, Vec<u8>) {
    let version = env!("CARGO_PKG_VERSION");
    let build = std::env::var("ALT_WEB_BUILD").unwrap_or_else(|_| "dev".to_string());
    let body = format!(
        "{{\"schema_version\":1,\"version\":\"{}\",\"build\":{}}}",
        version,
        json_string(&build)
    );
    (200, body.into_bytes())
}

/// `GET /api/stats` — repo-level counts the landing page surfaces.
/// Currently returns the count of refs as a single integer. Walks of
/// commit history live in the changelog endpoint; this one stays cheap.
pub fn handle_stats(src: &Source) -> Result<(u16, Vec<u8>), ApiError> {
    let repo = src.open()?;
    let refs = repo
        .list_refs()
        .map_err(|e| ApiError::Internal(format!("list_refs: {e}")))?;
    let head_oid = repo
        .rev_parse("HEAD")
        .map_err(|e| ApiError::Internal(format!("rev_parse HEAD: {e}")))?
        .map(|o| o.to_string())
        .unwrap_or_else(|| String::from("unknown"));

    let body = format!(
        "{{\"schema_version\":1,\"refs\":{},\"head\":{}}}",
        refs.len(),
        json_string(&head_oid)
    );
    Ok((200, body.into_bytes()))
}

/// `GET /api/changelog` — the most recent N commits on HEAD as
/// `{"schema_version":1, "commits":[{oid, subject}, ...]}`.
///
/// `n` is capped at 50 so the endpoint can never become a denial-of-service
/// foothold: a 500-deep commit walk is still microseconds, but capping
/// makes the response size predictable and the contract obvious.
pub fn handle_changelog(src: &Source, n: usize) -> Result<(u16, Vec<u8>), ApiError> {
    const MAX: usize = 50;
    let n = n.min(MAX);
    let repo = src.open()?;
    let head = match repo
        .rev_parse("HEAD")
        .map_err(|e| ApiError::Internal(format!("rev_parse HEAD: {e}")))?
    {
        Some(o) => o,
        None => {
            // No HEAD yet — empty list is the honest answer.
            return Ok((200, b"{\"schema_version\":1,\"commits\":[]}".to_vec()));
        }
    };

    let walked: Vec<_> = repo
        .rev_walk(head)
        .map_err(|e| ApiError::Internal(format!("rev_walk: {e}")))?
        .take(n)
        .collect::<Result<_, _>>()
        .map_err(|e| ApiError::Internal(format!("rev_walk: {e}")))?;
    let mut commits: Vec<String> = Vec::with_capacity(walked.len());
    for (oid, commit) in walked {
        // BString → utf-8 lossy String; subjects are git's first line.
        let raw = String::from_utf8_lossy(commit.message().as_slice()).into_owned();
        let subject = subject_of(&raw);
        commits.push(format!(
            "{{\"oid\":\"{}\",\"subject\":{}}}",
            oid,
            json_string(subject)
        ));
    }
    let body = format!(
        "{{\"schema_version\":1,\"commits\":[{}]}}",
        commits.join(",")
    );
    Ok((200, body.into_bytes()))
}

/// First line of a commit message; subjects are the only line a landing
/// page needs to render.
fn subject_of(message: &str) -> &str {
    match message.find('\n') {
        Some(p) => &message[..p],
        None => message,
    }
}

/// Hand-rolled JSON string encoder — the surface is small and stable, so
/// avoiding `serde_json` keeps the dep tree to `tiny_http + alt_repo`.
/// Escapes `"`, `\`, control characters; everything else (including
/// non-ASCII) is passed through, since the project's commit subjects are
/// UTF-8 by contract.
pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Helper for tests + internal callers: peek the alt store via [`Repository`].
#[doc(hidden)]
pub fn open_for_test(src: &Source) -> Result<Repository, ApiError> {
    src.open()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_string_escapes_quote_and_backslash() {
        assert_eq!(json_string(r#"hi"there"#), r#""hi\"there""#);
        assert_eq!(json_string("a\\b"), r#""a\\b""#);
        assert_eq!(json_string("first\nsecond"), r#""first\nsecond""#);
        assert_eq!(json_string("tab\there"), r#""tab\there""#);
    }

    #[test]
    fn json_string_passes_unicode_through() {
        let s = json_string("こんにちは alt");
        assert!(s.contains("こんにちは alt"), "got {s}");
    }

    #[test]
    fn subject_takes_first_line() {
        assert_eq!(subject_of("hello\nbody\nrest"), "hello");
        assert_eq!(subject_of("oneliner"), "oneliner");
        assert_eq!(subject_of(""), "");
    }

    #[test]
    fn handle_version_emits_stable_shape() {
        let (status, body) = handle_version();
        assert_eq!(status, 200);
        let s = String::from_utf8(body).unwrap();
        assert!(s.contains("\"schema_version\":1"), "got {s}");
        assert!(s.contains("\"version\":\"0.0.0\""), "got {s}");
        assert!(s.contains("\"build\":"), "got {s}");
    }
}
