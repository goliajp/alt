//! Checks run on what a commit is about to record, before it is written:
//! secrets in the lines it adds, and files too large for git hosts to take.
//! Pure logic over the changed files' old and new bytes.

use std::sync::LazyLock;

use regex::Regex;

/// Findings of this severity stop the commit; warnings are reported only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn word(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub validator: &'static str,
    pub rule: &'static str,
    pub severity: Severity,
    pub path: String,
    /// 1-based line in the new content, for findings about a line.
    pub line: Option<usize>,
    pub message: String,
}

/// One file the commit changes: its content in HEAD (`None` when new) and
/// in the index.
pub struct Change<'a> {
    pub path: &'a str,
    pub old: Option<&'a [u8]>,
    pub new: &'a [u8],
}

/// Files above this size draw a warning: alt stores them fine, but git hosts
/// cap file sizes (GitHub refuses files over 100 MiB) and slow down well
/// before that.
pub const LARGE_FILE_BYTES: usize = 10 * 1024 * 1024;

/// A line carrying this marker is exempt from the secret scan (fixtures,
/// documented example keys).
pub const ALLOW_MARKER: &str = "alt:allow-secret";

/// Runs every check over the changes.
pub fn check(changes: &[Change]) -> Vec<Finding> {
    let mut out = Vec::new();
    for c in changes {
        out.extend(scan_secrets(c));
        if c.new.len() > LARGE_FILE_BYTES {
            out.push(Finding {
                validator: "large-file-guard",
                rule: "large-file",
                severity: Severity::Warning,
                path: c.path.to_owned(),
                line: None,
                message: format!(
                    "{} MiB: fine in alt, but git hosts may refuse it on push or export \
                     (GitHub rejects files over 100 MiB)",
                    c.new.len() / (1024 * 1024)
                ),
            });
        }
    }
    out
}

struct Rule {
    name: &'static str,
    what: &'static str,
    re: Regex,
}

/// Credentials with a recognizable shape. Names stay generic on purpose.
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let rule = |name, what, re: &str| Rule {
        name,
        what,
        re: Regex::new(re).expect("built-in pattern"),
    };
    vec![
        rule(
            "private-key",
            "a private key",
            r"-----BEGIN [A-Z ]*PRIVATE KEY( BLOCK)?-----",
        ),
        rule(
            "cloud-access-key",
            "a cloud access key id",
            r"\b(AKIA|ASIA)[0-9A-Z]{16}\b",
        ),
        rule(
            "forge-token",
            "a code-hosting access token",
            r"\b(gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})\b",
        ),
        rule(
            "sk-api-key",
            "an API secret key",
            r"\bsk-(proj-|ant-)?[A-Za-z0-9_-]{32,}",
        ),
        rule(
            "chat-token",
            "a chat workspace token",
            r"\bxox[baprs]-[A-Za-z0-9-]{10,}",
        ),
        rule(
            "browser-api-key",
            "a web API key",
            r"\bAIza[0-9A-Za-z_-]{35}\b",
        ),
        rule(
            "payment-live-key",
            "a live payment key",
            r"\b[sr]k_live_[0-9A-Za-z]{24,}\b",
        ),
    ]
});

/// A value assigned to a secret-sounding name. Only flagged when the value
/// itself looks random (see [`looks_random`]), so identifiers, calls and
/// hashes named otherwise pass.
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)[a-z0-9_.-]*(api[_-]?key|secret|token|passw(or)?d|private[_-]?key|access[_-]?key)[a-z0-9_.-]*["']?\s*[:=]\s*["']?([A-Za-z0-9+/=_.-]{16,})"#,
    )
    .expect("built-in pattern")
});

/// Scans the lines `c` adds; content already in HEAD is left alone.
fn scan_secrets(c: &Change) -> Vec<Finding> {
    if alt_diff::is_binary(c.new) {
        return Vec::new();
    }
    let new = alt_diff::split_lines(c.new);
    let added: Vec<usize> = match c.old {
        None => (0..new.len()).collect(),
        Some(old) => {
            let old = alt_diff::split_lines(old);
            alt_diff::diff_lines(&old, &new)
                .into_iter()
                .flat_map(|e| e.new)
                .collect()
        }
    };
    let mut out = Vec::new();
    for i in added {
        let line = String::from_utf8_lossy(new[i]);
        if line.contains(ALLOW_MARKER) {
            continue;
        }
        let hit = RULES
            .iter()
            .find_map(|r| {
                r.re.find(&line)
                    .map(|m| (r.name, r.what, m.as_str().to_owned()))
            })
            .or_else(|| {
                ASSIGNMENT.captures(&line).and_then(|cap| {
                    let value = cap.get(3)?.as_str();
                    looks_random(value).then(|| {
                        (
                            "secret-assignment",
                            "a secret-looking value",
                            value.to_owned(),
                        )
                    })
                })
            });
        if let Some((rule, what, found)) = hit {
            out.push(Finding {
                validator: "secret-scan",
                rule,
                severity: Severity::Error,
                path: c.path.to_owned(),
                line: Some(i + 1),
                message: format!(
                    "{what} ({}); remove it and rotate it, or mark a deliberate \
                     fixture line with `{ALLOW_MARKER}`",
                    redact(&found)
                ),
            });
        }
    }
    out
}

/// Random enough to be a credential rather than a word or a placeholder:
/// letters and digits mixed, high per-character entropy, no filler.
fn looks_random(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let filler = [
        "xxxx",
        "example",
        "changeme",
        "your",
        "dummy",
        "placeholder",
        "redacted",
    ];
    if filler.iter().any(|f| lower.contains(f)) {
        return false;
    }
    let has_digit = value.bytes().any(|b| b.is_ascii_digit());
    let has_alpha = value.bytes().any(|b| b.is_ascii_alphabetic());
    has_digit && has_alpha && entropy(value) >= 3.5
}

/// Shannon entropy in bits per character.
fn entropy(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let n = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Enough of a match to find it, not enough to reuse it.
fn redact(s: &str) -> String {
    let shown: String = s.chars().take(6).collect();
    format!("{shown}…")
}

/// The findings as people read them.
pub fn render(findings: &[Finding]) -> String {
    let mut out = String::new();
    for f in findings {
        let at = match f.line {
            Some(l) => format!("{}:{l}", f.path),
            None => f.path.clone(),
        };
        out.push_str(&format!(
            "{} [{}/{}] {at}: {}\n",
            f.severity.word(),
            f.validator,
            f.rule,
            f.message
        ));
    }
    out
}

/// The findings as the fields of a stable JSON document for agents.
pub fn to_json(findings: &[Finding]) -> Vec<(&'static str, crate::json::Json)> {
    use crate::json::Json;
    vec![
        (
            "ok",
            Json::Bool(!findings.iter().any(|f| f.severity == Severity::Error)),
        ),
        (
            "findings",
            Json::Array(
                findings
                    .iter()
                    .map(|f| {
                        Json::Object(vec![
                            ("validator", Json::str(f.validator)),
                            ("rule", Json::str(f.rule)),
                            ("severity", Json::str(f.severity.word())),
                            ("path", Json::str(&f.path)),
                            ("line", f.line.map_or(Json::Null, |l| Json::Num(l as i64))),
                            ("message", Json::str(&f.message)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(new: &str) -> Vec<&'static str> {
        check(&[Change {
            path: "f",
            old: None,
            new: new.as_bytes(),
        }])
        .into_iter()
        .map(|f| f.rule)
        .collect()
    }

    // fake keys are assembled at run time so this file itself stays clean
    fn fake(prefix: &str, body: &str) -> String {
        format!("{prefix}{body}")
    }

    #[test]
    fn shaped_credentials_are_caught() {
        let key = fake("AKIA", "ABCDEFGHIJKLMNOP");
        assert_eq!(scan(&format!("id = {key}\n")), ["cloud-access-key"]);
        let pem = fake("-----BEGIN RSA ", "PRIVATE KEY-----");
        assert_eq!(scan(&format!("{pem}\nMII\n")), ["private-key"]);
        let tok = fake("ghp_", &"a1B2".repeat(10));
        assert_eq!(scan(&format!("x: {tok}\n")), ["forge-token"]);
    }

    #[test]
    fn assignments_count_only_when_the_value_looks_random() {
        let v = fake("Zq8", "xK2vP9mL4rT7wN3s");
        assert_eq!(scan(&format!("API_KEY={v}\n")), ["secret-assignment"]);
        assert!(scan("let token = self.next_token();\n").is_empty());
        assert!(scan("password = \"your_password_here_123\"\n").is_empty());
        assert!(scan("api_key: xxxxxxxxxxxxxxxxxxxx\n").is_empty());
        // a hash that is not assigned to a secret-sounding name is data
        assert!(scan("checksum = \"d2f6c7dbe95a6ed67ad9f18e57daf93a2f034c52\"\n").is_empty());
    }

    #[test]
    fn only_added_lines_are_scanned_and_the_marker_exempts_a_line() {
        let key = fake("AKIA", "ABCDEFGHIJKLMNOP");
        let old = format!("a\n{key}\n");
        let new = format!("a\n{key}\nb\n");
        let found = check(&[Change {
            path: "f",
            old: Some(old.as_bytes()),
            new: new.as_bytes(),
        }]);
        assert!(found.is_empty(), "{found:?}");
        assert!(scan(&format!("{key} # {ALLOW_MARKER}\n")).is_empty());
    }

    #[test]
    fn large_files_warn_without_stopping() {
        let big = vec![b'a'; LARGE_FILE_BYTES + 1];
        let found = check(&[Change {
            path: "big",
            old: None,
            new: &big,
        }]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].severity, Severity::Warning);
    }
}
