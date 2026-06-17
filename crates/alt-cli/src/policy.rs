//! Repository capability policy (A6).
//!
//! A [`Policy`] is a list of `(principal-glob, [`Capabilities`])` rules loaded
//! from `<alt-dir>/policy` (plain text, zero-dep parser — same调性 as
//! [`crate::json`]). On every write the repo asks `policy.lookup(&principal)`
//! and the resulting [`Capabilities`] is what the gates in C3 will check
//! against (ref namespace + read-only inside `RefStore`; force + path inside
//! `NativeRepo`). C2 is the model + loader + lookup; the gates come in C3.
//!
//! ## File format
//!
//! ```text
//! # comments and blank lines are skipped
//! <principal-glob> -> <cap-spec>
//! ```
//!
//! The principal-glob matches the same canonical form the op log carries —
//! `<kind>:<id>` (`agent:claude-opus-4-8`, `human:alice`) — so what you read
//! in the audit log is what you write in the policy. Globs use two tokens:
//! `*` matches one segment (no `/`), `**` matches anything including `/`.
//!
//! cap-spec is whitespace-separated tokens:
//!
//! | token | effect |
//! |-------|--------|
//! | `read-only` | blocks every write |
//! | `forbid-force` | non-fast-forward updates and branch deletion are denied |
//! | `branch=<glob>` | restricts ref writes to matches (repeatable; accumulates) |
//! | `path=<glob>` | restricts stage/commit paths to matches (repeatable) |
//!
//! An omitted allow-list means "no constraint along that axis" (empty == any),
//! so a rule with no tokens is identical to having no rule at all. This keeps
//! the *absence* of policy file (or the *absence* of a matching rule) a true
//! zero-regression default — none of today's behaviour changes until someone
//! explicitly writes a restrictive rule.
//!
//! ## Matching
//!
//! Rules are tried top-to-bottom; the **first** matching principal-glob wins
//! and its [`Capabilities`] are returned. No match → [`Capabilities::full`]
//! (everything allowed). First-match-wins keeps the model simple and lets
//! operators put specific rules (`agent:claude-*`) above broad ones (`agent:*`).

use std::fs;
use std::io;
use std::path::Path;

use crate::native::Principal;

/// What a principal may do at the repo level. The fields are *allow* axes —
/// empty lists mean "no constraint on this axis". See the module docs for the
/// surface semantics; the gates are C3 (ref namespace + read-only in
/// `RefStore`, force + path in `NativeRepo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Block every write — refs, objects, and the index.
    pub read_only: bool,
    /// Allow-list for ref names; empty = any ref allowed.
    pub branch_allow: Vec<Glob>,
    /// Allow-list for working-tree paths under `add`/`commit`; empty = any.
    pub path_allow: Vec<Glob>,
    /// Deny non-fast-forward updates and branch deletion.
    pub forbid_force: bool,
    /// M10/W14 (A5b): when set, the wire receive-pack path rejects any
    /// push that doesn't carry a valid Ed25519 signature over the
    /// canonical push payload. Only enforced on the wire — local CLI
    /// writes don't traverse the signature path.
    pub require_signed: bool,
}

impl Capabilities {
    /// The zero-regression default: no constraint anywhere. Used when no
    /// policy file exists, or when no rule matches the principal.
    pub fn full() -> Self {
        Self {
            read_only: false,
            branch_allow: Vec::new(),
            path_allow: Vec::new(),
            forbid_force: false,
            require_signed: false,
        }
    }

    /// `true` iff a ref named `name` may be written. Empty `branch_allow`
    /// means "no constraint"; `read_only` overrides allow-lists.
    pub fn allows_branch(&self, name: &str) -> bool {
        if self.read_only {
            return false;
        }
        self.allows_branch_name(name)
    }

    /// Namespace-only check (ignores `read_only`). Used by the ref-store gate,
    /// which reports the read-only deny separately from the namespace deny so
    /// the error message can name *which* constraint fired.
    pub fn allows_branch_name(&self, name: &str) -> bool {
        if self.branch_allow.is_empty() {
            return true;
        }
        self.branch_allow.iter().any(|g| g.matches(name))
    }

    /// `true` iff a working-tree path may be staged/committed. Empty
    /// `path_allow` means "no constraint"; `read_only` overrides.
    pub fn allows_path(&self, path: &str) -> bool {
        if self.read_only {
            return false;
        }
        self.allows_path_name(path)
    }

    /// Path-only check (ignores `read_only`). See [`allows_branch_name`] for
    /// why the read-only axis is separated out.
    pub fn allows_path_name(&self, path: &str) -> bool {
        if self.path_allow.is_empty() {
            return true;
        }
        self.path_allow.iter().any(|g| g.matches(path))
    }
}

/// A repository-level policy: an ordered list of `(principal-glob, caps)`
/// rules. Order matters — see [`Self::lookup`].
#[derive(Debug, Clone, Default)]
pub struct Policy {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
struct Rule {
    principal: Glob,
    caps: Capabilities,
}

impl Policy {
    /// An empty policy: every principal gets [`Capabilities::full`]. Returned
    /// by [`load`] when `<alt-dir>/policy` does not exist.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load and parse `<alt-dir>/policy`. A missing file is normal and yields
    /// an empty policy (zero-regression); a malformed file is a hard error so
    /// the operator sees the typo instead of a silently weakened gate.
    pub fn load(alt_dir: &Path) -> Result<Self, PolicyError> {
        let path = alt_dir.join("policy");
        let text = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(e) => return Err(PolicyError::Io(e)),
        };
        Self::parse(&text)
    }

    /// Parse policy text directly — exposed for tests and for callers that
    /// hold the text in memory.
    pub fn parse(text: &str) -> Result<Self, PolicyError> {
        let mut rules = Vec::new();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((lhs, rhs)) = line.split_once("->") else {
                return Err(PolicyError::Syntax {
                    line: lineno + 1,
                    msg: "missing '->' between principal-glob and capability-spec".into(),
                });
            };
            let principal = Glob::new(lhs.trim());
            let caps = parse_caps(rhs.trim(), lineno + 1)?;
            rules.push(Rule { principal, caps });
        }
        Ok(Self { rules })
    }

    /// Return the capabilities for `principal`. **First match wins**: rules
    /// are scanned in file order; the first whose principal-glob matches
    /// `<kind>:<id>` provides the result. No match → [`Capabilities::full`].
    pub fn lookup(&self, principal: &Principal) -> Capabilities {
        let target = principal_target(principal);
        for r in &self.rules {
            if r.principal.matches(&target) {
                return r.caps.clone();
            }
        }
        Capabilities::full()
    }
}

fn principal_target(p: &Principal) -> String {
    let kind = match p.kind {
        crate::native::PrincipalKind::Human => "human",
        crate::native::PrincipalKind::Agent => "agent",
    };
    format!("{kind}:{}", p.id)
}

fn parse_caps(spec: &str, line: usize) -> Result<Capabilities, PolicyError> {
    let mut caps = Capabilities::full();
    for tok in spec.split_whitespace() {
        if tok == "read-only" {
            caps.read_only = true;
        } else if tok == "forbid-force" {
            caps.forbid_force = true;
        } else if tok == "require-signed" {
            caps.require_signed = true;
        } else if let Some(v) = tok.strip_prefix("branch=") {
            caps.branch_allow.push(Glob::new(v));
        } else if let Some(v) = tok.strip_prefix("path=") {
            caps.path_allow.push(Glob::new(v));
        } else {
            return Err(PolicyError::Syntax {
                line,
                msg: format!("unknown capability token: {tok:?}"),
            });
        }
    }
    Ok(caps)
}

/// Parse / load errors.
#[derive(Debug)]
pub enum PolicyError {
    Io(io::Error),
    Syntax { line: usize, msg: String },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "policy file io error: {e}"),
            Self::Syntax { line, msg } => write!(f, "policy: line {line}: {msg}"),
        }
    }
}

impl std::error::Error for PolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(e) = self {
            Some(e)
        } else {
            None
        }
    }
}

/// A two-token glob: `*` matches one segment (no `/`), `**` matches anything
/// (including `/`). Plain text otherwise — no escapes. Stored compiled into a
/// list of literal/anysegment/anychars parts so matching is a flat walk and
/// not a regex engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit(String),
    AnySegment, // `*`
    AnyChars,   // `**`
}

impl Glob {
    /// Compile a glob string. `**` absorbs adjacent `/` on both sides so that
    /// `a/**/b` matches `a/b` (gitignore semantics) — the "0 or more path
    /// segments" reading where the separators around the wildcard are part of
    /// the wildcard, not a hard literal you must reproduce.
    pub fn new(pat: &str) -> Self {
        let bytes = pat.as_bytes();
        let mut parts = Vec::new();
        let mut buf = String::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'*' {
                let double = i + 1 < bytes.len() && bytes[i + 1] == b'*';
                // `**` adjacent to `/` on the left: drop the trailing slash
                // from the literal so it counts as part of the wildcard span.
                if double && buf.ends_with('/') {
                    buf.pop();
                }
                if !buf.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut buf)));
                }
                if double {
                    parts.push(Part::AnyChars);
                    i += 2;
                    // …and on the right: skip a trailing `/` after `**`.
                    if i < bytes.len() && bytes[i] == b'/' {
                        i += 1;
                    }
                } else {
                    parts.push(Part::AnySegment);
                    i += 1;
                }
            } else {
                buf.push(bytes[i] as char);
                i += 1;
            }
        }
        if !buf.is_empty() {
            parts.push(Part::Lit(buf));
        }
        Self { parts }
    }

    /// Match the whole `input` against this glob.
    pub fn matches(&self, input: &str) -> bool {
        match_parts(&self.parts, input.as_bytes())
    }
}

fn match_parts(parts: &[Part], input: &[u8]) -> bool {
    let mut i = 0; // input cursor
    let mut p = 0; // parts cursor
    // The classic backtracking matcher: `**` ("any chars") may need to try
    // multiple split points before what follows succeeds; `*` (one segment)
    // is greedy up to the next `/`. Patterns here are tiny — backtracking is
    // simpler than a NFA and there is no perf pressure on this path.
    let mut bt: Option<(usize, usize)> = None; // (parts_idx of `**`, next input_pos to try)
    loop {
        if p == parts.len() {
            if i == input.len() {
                return true;
            }
            // unmatched tail — fall through to backtrack
        } else {
            match &parts[p] {
                Part::Lit(s) => {
                    let s = s.as_bytes();
                    if i + s.len() <= input.len() && &input[i..i + s.len()] == s {
                        i += s.len();
                        p += 1;
                        continue;
                    }
                }
                Part::AnySegment => {
                    // greedy up to next `/`; record the next part for the
                    // matcher to verify after each candidate length
                    let end = input[i..]
                        .iter()
                        .position(|&b| b == b'/')
                        .map(|k| i + k)
                        .unwrap_or(input.len());
                    // try the longest match first; backtrack one byte at a
                    // time on failure (segment lengths are short).
                    return match_anysegment(parts, p, input, i, end);
                }
                Part::AnyChars => {
                    bt = Some((p, i));
                    p += 1;
                    continue;
                }
            }
        }
        // mismatch / overflow — try to backtrack into a prior `**`
        if let Some((bp, bi)) = bt
            && bi < input.len()
        {
            i = bi + 1;
            bt = Some((bp, i));
            p = bp + 1;
            continue;
        }
        return false;
    }
}

fn match_anysegment(parts: &[Part], p: usize, input: &[u8], lo: usize, hi: usize) -> bool {
    // `*` requires at least one byte (it stands for a non-empty path segment),
    // so refuse the empty-segment case up front.
    if hi == lo {
        return false;
    }
    // try every split from longest to single-byte (greedy, then backtrack)
    let rest = &parts[p + 1..];
    let mut end = hi;
    loop {
        if match_parts(rest, &input[end..]) {
            return true;
        }
        if end == lo + 1 {
            return false;
        }
        end -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{Principal, PrincipalKind};

    fn principal(kind: PrincipalKind, id: &str) -> Principal {
        Principal {
            kind,
            id: id.into(),
            session: None,
        }
    }

    #[test]
    fn require_signed_token_lifts_into_capabilities() {
        let p = Policy::parse("human:anonymous -> require-signed").unwrap();
        let caps = p.lookup(&principal(PrincipalKind::Human, "anonymous"));
        assert!(
            caps.require_signed,
            "require-signed token must set the flag"
        );
        let other = p.lookup(&principal(PrincipalKind::Human, "alice"));
        assert!(
            !other.require_signed,
            "non-matching principal must fall through to default"
        );
    }

    #[test]
    fn glob_literals_and_single_segment() {
        assert!(Glob::new("refs/heads/main").matches("refs/heads/main"));
        assert!(!Glob::new("refs/heads/main").matches("refs/heads/maint"));
        let g = Glob::new("refs/heads/*");
        assert!(g.matches("refs/heads/main"));
        assert!(g.matches("refs/heads/feature"));
        assert!(!g.matches("refs/heads/feature/x"), "* must not cross /");
        assert!(!g.matches("refs/heads/"), "* requires at least one char");
    }

    #[test]
    fn glob_double_star_crosses_slashes() {
        let g = Glob::new("refs/heads/feature/agent-claude/**");
        assert!(g.matches("refs/heads/feature/agent-claude/x"));
        assert!(g.matches("refs/heads/feature/agent-claude/nested/branch"));
        assert!(g.matches("refs/heads/feature/agent-claude/"));
        assert!(!g.matches("refs/heads/feature/other/x"));

        // `**` between two literals — must still backtrack to find a match
        let g = Glob::new("src/**/lib.rs");
        assert!(g.matches("src/lib.rs"));
        assert!(g.matches("src/a/lib.rs"));
        assert!(g.matches("src/a/b/c/lib.rs"));
        assert!(!g.matches("src/a/lib.rsx"));
    }

    #[test]
    fn empty_policy_is_full_capabilities() {
        let p = Policy::empty();
        let alice = principal(PrincipalKind::Human, "alice");
        let caps = p.lookup(&alice);
        assert_eq!(caps, Capabilities::full());
        assert!(caps.allows_branch("refs/heads/main"));
        assert!(caps.allows_path("src/lib.rs"));
        assert!(!caps.read_only);
        assert!(!caps.forbid_force);
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let p =
            Policy::parse("# a comment\n\nagent:* -> read-only\n\n# trailing comment\n").unwrap();
        let agent = principal(PrincipalKind::Agent, "x");
        assert!(p.lookup(&agent).read_only);
        assert!(
            !p.lookup(&principal(PrincipalKind::Human, "alice"))
                .read_only
        );
    }

    #[test]
    fn first_match_wins() {
        // `agent:claude-*` is specific; `agent:*` is the broad catch-all
        let p = Policy::parse(
            "agent:claude-* -> branch=refs/heads/feature/claude/**\n\
             agent:*        -> read-only\n",
        )
        .unwrap();
        let claude = principal(PrincipalKind::Agent, "claude-opus");
        let caps = p.lookup(&claude);
        assert!(!caps.read_only, "specific rule wins, not the read-only one");
        assert!(caps.allows_branch("refs/heads/feature/claude/x"));
        assert!(!caps.allows_branch("refs/heads/main"));

        let other = principal(PrincipalKind::Agent, "rover");
        assert!(
            p.lookup(&other).read_only,
            "catch-all applies to non-claude"
        );

        // Order swap: now the catch-all comes first and shadows the specific
        let p = Policy::parse(
            "agent:*        -> read-only\n\
             agent:claude-* -> branch=refs/heads/feature/claude/**\n",
        )
        .unwrap();
        assert!(
            p.lookup(&claude).read_only,
            "first match wins: catch-all above specific shadows it"
        );
    }

    #[test]
    fn cap_spec_accumulates_branch_and_path() {
        let p = Policy::parse(
            "agent:claude -> branch=refs/heads/feature/** path=src/** path=docs/**\n",
        )
        .unwrap();
        let caps = p.lookup(&principal(PrincipalKind::Agent, "claude"));
        assert_eq!(caps.branch_allow.len(), 1);
        assert_eq!(caps.path_allow.len(), 2);
        assert!(caps.allows_path("src/lib.rs"));
        assert!(caps.allows_path("docs/readme.md"));
        assert!(!caps.allows_path("bin/script"));
    }

    #[test]
    fn read_only_overrides_allow_lists() {
        let p = Policy::parse("agent:* -> read-only branch=refs/heads/**\n").unwrap();
        let caps = p.lookup(&principal(PrincipalKind::Agent, "x"));
        assert!(caps.read_only);
        assert!(
            !caps.allows_branch("refs/heads/main"),
            "read-only beats any allow-list"
        );
        assert!(!caps.allows_path("anywhere"));
    }

    #[test]
    fn forbid_force_token_sets_flag() {
        let p = Policy::parse("human:* -> forbid-force\n").unwrap();
        let caps = p.lookup(&principal(PrincipalKind::Human, "alice"));
        assert!(caps.forbid_force);
        assert!(!caps.read_only);
        assert!(caps.allows_branch("refs/heads/main"), "no allow-list = any");
    }

    #[test]
    fn unknown_token_is_a_hard_error() {
        let err = Policy::parse("agent:* -> wat\n").unwrap_err();
        match err {
            PolicyError::Syntax { line, msg } => {
                assert_eq!(line, 1);
                assert!(msg.contains("wat"), "msg should name the bad token: {msg}");
            }
            other => panic!("expected Syntax error, got {other:?}"),
        }
    }

    #[test]
    fn missing_arrow_is_a_hard_error() {
        let err = Policy::parse("agent:* read-only\n").unwrap_err();
        assert!(matches!(err, PolicyError::Syntax { line: 1, .. }));
    }

    #[test]
    fn missing_file_yields_empty_policy_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Policy::load(tmp.path()).unwrap();
        // empty policy → full capabilities for everyone
        assert_eq!(
            p.lookup(&principal(PrincipalKind::Agent, "x")),
            Capabilities::full()
        );
    }

    #[test]
    fn loads_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("policy"), "agent:* -> read-only\n").unwrap();
        let p = Policy::load(tmp.path()).unwrap();
        assert!(p.lookup(&principal(PrincipalKind::Agent, "x")).read_only);
        assert!(
            !p.lookup(&principal(PrincipalKind::Human, "alice"))
                .read_only
        );
    }

    #[test]
    fn principal_kind_is_part_of_the_target() {
        // a glob that requires `agent:` prefix should not match a human
        let p = Policy::parse("agent:* -> read-only\n").unwrap();
        assert!(p.lookup(&principal(PrincipalKind::Agent, "any")).read_only);
        assert!(!p.lookup(&principal(PrincipalKind::Human, "any")).read_only);
    }
}
