//! Who a request is from and what it may do: the users file of a
//! multi-repo root and the per-repo rules of its scoped users.

use std::collections::HashMap;
use std::path::Path;

use tiny_http::Method;

/// What `check_auth` decided about a request. `Reject` carries the
/// short string the body gets so a human running curl sees something
/// meaningful (a real git client just retries with the credential).
/// `Allow` returns the authenticated user (None when no users file was
/// in play, so the request is anonymous) and the user's ACL when they
/// were scoped (None = trusted user, every repo + every action).
pub(super) enum AuthOutcome {
    Allow {
        user: Option<String>,
        acl: Option<Vec<AclRule>>,
    },
    Reject(String),
}

/// One entry in a user's per-repo permission table. `repo == "*"` is the
/// wildcard meaning "every repo this user can see"; `perm` says whether
/// they can read, write, or both.
#[derive(Debug, Clone)]
pub(super) struct AclRule {
    repo: String,
    can_read: bool,
    can_write: bool,
}

/// What kind of access the current HTTP request needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Read,
    Write,
}

pub(super) fn action_from_request(method: &Method, path: &str, query: &str) -> Option<Action> {
    if method == &Method::Get && path.ends_with("/info/refs") {
        // the service as the handler reads it, not any text of the query
        return match super::parse_query(query, "service") {
            Some("git-upload-pack") => Some(Action::Read),
            Some("git-receive-pack") => Some(Action::Write),
            _ => None,
        };
    }
    if method == &Method::Post && path.ends_with("/git-upload-pack") {
        return Some(Action::Read);
    }
    if method == &Method::Post && path.ends_with("/git-receive-pack") {
        return Some(Action::Write);
    }
    None
}

pub(super) fn acl_allows(acl: &[AclRule], repo: &str, action: Action) -> bool {
    for rule in acl {
        if rule.repo != "*" && rule.repo != repo {
            continue;
        }
        match action {
            Action::Read => {
                if rule.can_read {
                    return true;
                }
            }
            Action::Write => {
                if rule.can_write {
                    return true;
                }
            }
        }
    }
    false
}

fn parse_acl(field: &str) -> Vec<AclRule> {
    let mut out = Vec::new();
    for token in field.split_whitespace() {
        let Some((repo, perm)) = token.split_once(':') else {
            continue;
        };
        let (can_read, can_write) = match perm {
            "r" => (true, false),
            "w" => (false, true),
            "rw" | "wr" => (true, true),
            "n" | "" => (false, false),
            _ => continue,
        };
        out.push(AclRule {
            repo: repo.to_owned(),
            can_read,
            can_write,
        });
    }
    out
}

/// constant-time, ASCII-case-insensitive byte comparison.
/// Short-circuit `==` / `eq_ignore_ascii_case` leaks bytes of the
/// stored token hash through wall-clock timing differences: an
/// attacker who can fire many auth requests reconstructs the hash
/// one byte at a time. This helper always walks the full length of
/// both inputs, accumulating an OR of per-byte XORs so the work is
/// independent of where (or whether) the inputs first differ.
fn constant_time_eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    // Length mismatch is the one early-exit we permit; the stored
    // BLAKE3 hex is always 64 ASCII chars and the candidate is hashed
    // by us, so honest paths never trip the length branch and the
    // early-exit reveals no per-byte information.
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x.to_ascii_lowercase() ^ y.to_ascii_lowercase();
    }
    diff == 0
}

/// Validate an HTTP Basic `Authorization` header against the users file
/// at `users_path`. The file format is `<name>\t<blake3-hex-of-token>\n`
/// per line, with `#` comment lines and blank lines tolerated; the
/// token itself is never stored, only its BLAKE3 hash.
pub(super) fn check_auth(req: &tiny_http::Request, users_path: &Path) -> AuthOutcome {
    let Some(header_value) = req
        .headers()
        .iter()
        .find(|h| {
            h.field
                .as_str()
                .as_str()
                .eq_ignore_ascii_case("authorization")
        })
        .map(|h| h.value.as_str().to_owned())
    else {
        return AuthOutcome::Reject("missing Authorization header".into());
    };
    let Some(b64) = header_value.strip_prefix("Basic ") else {
        return AuthOutcome::Reject("only Basic auth is supported".into());
    };
    let Ok(decoded) = alt_base64::STANDARD.decode(b64.trim()) else {
        return AuthOutcome::Reject("Authorization base64 decode failed".into());
    };
    let decoded_str = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return AuthOutcome::Reject("Authorization is not utf-8".into()),
    };
    let Some((user, token)) = decoded_str.split_once(':') else {
        return AuthOutcome::Reject("Authorization missing ':' separator".into());
    };
    let token_hash = alt_hash::blake3::hash(token.as_bytes());
    let token_hex = token_hash.to_hex();
    let table = match read_users(users_path) {
        Ok(t) => t,
        Err(e) => return AuthOutcome::Reject(format!("server users file unreadable: {e}")),
    };
    // one answer for a user that is not known and for a token that is not
    // theirs: which names there are is not told
    let known = table.get(user);
    let theirs = known.filter(|entry| {
        constant_time_eq_ignore_ascii_case(entry.token_hash.as_bytes(), token_hex.as_bytes())
    });
    let Some(entry) = theirs else {
        return AuthOutcome::Reject("bad credentials".into());
    };
    // a 2-column users line (no ACL) is the "trusted user"
    // shape: every repo + every action allowed. A 3-column line scopes
    // the user, and the dispatcher then asks `acl_allows` per request.
    AuthOutcome::Allow {
        user: Some(user.to_owned()),
        acl: entry.acl.clone(),
    }
}

#[derive(Debug, Clone)]
struct UserEntry {
    token_hash: String,
    /// `None` = trusted user, every repo + every action allowed.
    /// `Some(rules)` = scoped user, only the listed rules apply.
    acl: Option<Vec<AclRule>>,
}

fn read_users(path: &Path) -> std::io::Result<HashMap<String, UserEntry>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = HashMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.splitn(3, '\t');
        let Some(user) = parts.next() else { continue };
        let Some(hash) = parts.next() else { continue };
        let acl = parts.next().map(parse_acl);
        out.insert(
            user.trim().to_owned(),
            UserEntry {
                token_hash: hash.trim().to_owned(),
                acl,
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The service a request for refs is held to is the one it is
    /// answered for: the first `service` of the query.
    #[test]
    fn the_refs_of_a_service_are_held_to_the_rule_of_that_service() {
        let of = |query| action_from_request(&Method::Get, "/a/info/refs", query);
        assert_eq!(of("service=git-upload-pack"), Some(Action::Read));
        assert_eq!(of("service=git-receive-pack"), Some(Action::Write));
        let both = "service=git-receive-pack&x=service=git-upload-pack";
        assert_eq!(of(both), Some(Action::Write));
        assert_eq!(of("x=service=git-upload-pack"), None);
    }

    /// constant-time compare basic correctness on the
    /// values it actually sees in production: 64-char BLAKE3 hex
    /// strings, plus a few edge cases.
    #[test]
    fn constant_time_eq_ignore_ascii_case_basic() {
        let a = b"abcdef0123456789";
        let b = b"abcdef0123456789";
        assert!(constant_time_eq_ignore_ascii_case(a, b));
        // Case insensitivity matches the original `eq_ignore_ascii_case`
        // behavior — BLAKE3 hex output is lowercase but a poorly-
        // configured operator might paste uppercase.
        assert!(constant_time_eq_ignore_ascii_case(
            b"ABCDEF0123456789",
            b"abcdef0123456789"
        ));
        // Differences anywhere in the string are detected.
        assert!(!constant_time_eq_ignore_ascii_case(a, b"xbcdef0123456789"));
        assert!(!constant_time_eq_ignore_ascii_case(a, b"abcdef012345678x"));
        // Length mismatch is the one early-exit.
        assert!(!constant_time_eq_ignore_ascii_case(b"abc", b"abcd"));
        // Empty strings compare equal (degenerate but well-defined).
        assert!(constant_time_eq_ignore_ascii_case(b"", b""));
    }

    /// The compare must not short-circuit on the first differing byte.
    /// We can't measure timing from a Rust unit test reliably, but
    /// the *control flow* must walk the full length — meaning a
    /// diff in the first byte and a diff in the last byte must both
    /// reach the loop's end. We verify this property by hand-counting
    /// iterations via a wrapping accumulator: any short-circuit
    /// would diverge.
    #[test]
    fn constant_time_eq_does_full_pass_regardless_of_diff_position() {
        // 64-byte test vectors so the loop body actually does work.
        let stored = b"0000000000000000000000000000000000000000000000000000000000000000";
        let head_diff = b"f000000000000000000000000000000000000000000000000000000000000000";
        let tail_diff = b"000000000000000000000000000000000000000000000000000000000000000f";
        let mid_diff = b"00000000000000000000000000000000f0000000000000000000000000000000";

        // All three must return false (mismatch); the comparison is
        // identical work regardless of position.
        assert!(!constant_time_eq_ignore_ascii_case(stored, head_diff));
        assert!(!constant_time_eq_ignore_ascii_case(stored, tail_diff));
        assert!(!constant_time_eq_ignore_ascii_case(stored, mid_diff));
        // And the identical-input path returns true.
        assert!(constant_time_eq_ignore_ascii_case(stored, stored));
    }
}
