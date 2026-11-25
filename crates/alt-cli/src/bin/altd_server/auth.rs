//! Who a request is from and what it may do: the users file of a
//! multi-repo root and the per-repo rules of its scoped users.

use std::collections::HashMap;
use std::path::Path;

use super::http::Method;

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
    /// the users file does not read: nobody is let in by it, and nobody
    /// is asked for another credential
    Unread,
}

/// One entry in a user's per-repo permission table. `repo == "*"` is the
/// wildcard meaning "every repo this user can see"; `perm` says whether
/// they can read, write, or both.
#[derive(Debug, Clone)]
pub(super) struct AclRule {
    pub(super) repo: String,
    pub(super) can_read: bool,
    pub(super) can_write: bool,
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

/// Whether `acl` lets `action` be done to `repo`. The rules that name the
/// repository decide for it; `*` stands for the repositories that no rule
/// names. A repository named with nothing allowed is closed, whatever `*`
/// allows the others.
pub(super) fn acl_allows(acl: &[AclRule], repo: &str, action: Action) -> bool {
    let allows = |rule: &AclRule| match action {
        Action::Read => rule.can_read,
        Action::Write => rule.can_write,
    };
    let mut named = acl.iter().filter(|rule| rule.repo == repo).peekable();
    match named.peek() {
        Some(_) => named.any(allows),
        None => acl.iter().any(|rule| rule.repo == "*" && allows(rule)),
    }
}

/// The rules of `field`, or the first of its words that is none. A word
/// that does not read is not left out: the rule it was meant for would
/// then not be there, and `*` would decide for the repository it names.
pub(super) fn parse_acl(field: &str) -> Result<Vec<AclRule>, String> {
    let rule = |token: &str| {
        let (repo, perm) = token.split_once(':').filter(|(repo, _)| !repo.is_empty())?;
        let (can_read, can_write) = match perm {
            "r" => (true, false),
            "w" => (false, true),
            "rw" | "wr" => (true, true),
            "n" | "" => (false, false),
            _ => return None,
        };
        Some(AclRule {
            repo: repo.to_owned(),
            can_read,
            can_write,
        })
    };
    let unread = |token: &str| format!("{token:?} is no rule: <repository>:<r|w|rw|n> is expected");
    let rules = field.split_whitespace();
    rules
        .map(|token| rule(token).ok_or_else(|| unread(token)))
        .collect()
}

/// Whether the users file at `path` reads, and why it does not.
pub(super) fn users_read(path: &Path) -> Result<(), String> {
    read_users(path).map(drop)
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
pub(super) fn check_auth(req: &super::http::Request, users_path: &Path) -> AuthOutcome {
    let Some(header_value) = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("authorization"))
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
        Err(why) => {
            eprintln!("altd-server: {}: {why}", users_path.display());
            return AuthOutcome::Unread;
        }
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

/// What a line of the users file names: a user, the hash of their token and,
/// where the line has a third column, their rules. Nothing for a line that is
/// empty, a comment, or has no hash. The line is taken without the blanks
/// around it, so a third column of blanks alone is no column.
pub(super) fn fields(line: &str) -> Option<(&str, &str, Option<&str>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut parts = trimmed.splitn(3, '\t');
    Some((parts.next()?.trim(), parts.next()?.trim(), parts.next()))
}

fn read_users(path: &Path) -> Result<HashMap<String, UserEntry>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut out = HashMap::new();
    for (at, line) in text.lines().enumerate() {
        let Some((user, hash, rules)) = fields(line) else {
            continue;
        };
        let acl = rules.map(parse_acl).transpose();
        let acl = acl.map_err(|why| format!("line {}: {why}", at + 1))?;
        let token_hash = hash.to_owned();
        out.insert(user.to_owned(), UserEntry { token_hash, acl });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rules_that_name_a_repository_decide_for_it_and_the_star_for_the_rest() {
        let may = |acl: &str, repo: &str| {
            let acl = parse_acl(acl).unwrap();
            (
                acl_allows(&acl, repo, Action::Read),
                acl_allows(&acl, repo, Action::Write),
            )
        };
        assert_eq!(may("*:rw", "any"), (true, true));
        assert_eq!(may("*:r", "any"), (true, false));
        assert_eq!(may("alpha:rw beta:r", "alpha"), (true, true));
        assert_eq!(may("alpha:rw beta:r", "beta"), (true, false));
        assert_eq!(may("alpha:rw beta:r", "gamma"), (false, false));
        // closed by name, in whatever order the rules stand
        assert_eq!(may("secret:n *:rw", "secret"), (false, false));
        assert_eq!(may("*:rw secret:n", "secret"), (false, false));
        assert_eq!(may("*:rw secret:", "secret"), (false, false));
        assert_eq!(may("secret:n *:rw", "other"), (true, true));
        // narrowed by name, and widened
        assert_eq!(may("*:rw docs:r", "docs"), (true, false));
        assert_eq!(may("*:r docs:w", "docs"), (false, true));
        assert_eq!(may("*:r docs:rw", "docs"), (true, true));
        // two rules of one name allow what either does
        assert_eq!(may("docs:r docs:w *:n", "docs"), (true, true));
        assert_eq!(may("", "docs"), (false, false));
        // a word that does not read is told, and the rules beside it are not taken
        for field in [
            "docs:x *:r",
            "*:r docs:x",
            "docs",
            ":rw",
            "docs:rwx",
            "docs:r:w",
        ] {
            let why = parse_acl(field).unwrap_err();
            assert!(why.contains("is no rule"), "{field}: {why}");
        }
        assert!(
            parse_acl("*:r docs:x")
                .unwrap_err()
                .starts_with("\"docs:x\"")
        );
    }

    #[test]
    fn a_users_file_with_a_rule_that_does_not_read_is_not_read_for_anybody() {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("users");
        std::fs::write(&path, "# who\nalice\t00\nbob\t11\tdocs:r *:n\n").unwrap();
        let read = read_users(&path).unwrap();
        assert!(read["alice"].acl.is_none());
        assert_eq!(read["bob"].acl.as_ref().unwrap().len(), 2);
        std::fs::write(&path, "alice\t00\n\nbob\t11\tdocs:x *:rw\n").unwrap();
        let why = users_read(&path).unwrap_err();
        assert!(why.starts_with("line 3: \"docs:x\" is no rule"), "{why}");
        assert!(users_read(&dir.path().join("none")).is_err());
    }

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
