//! Repository capability policy.
//!
//! A [`Policy`] is a list of `(principal-glob, [`Capabilities`])` rules loaded
//! from `<alt-dir>/policy` (plain text, zero-dep parser — same style as
//! [`crate::json`]). On every write the repo asks `policy.lookup(&principal)`
//! and the resulting [`Capabilities`] is what the gates check
//! against (ref namespace + read-only inside `RefStore`; force + path inside
//! `NativeRepo`).
//!
//! ## File format
//!
//! ```text
//! # comments and blank lines are skipped
//! <principal-glob> -> <cap-spec>
//! ```
//!
//! The principal-glob matches the same canonical form the op log carries —
//! `<kind>:<id>` (`agent:bot-1`, `human:alice`) — so what you read
//! in the audit log is what you write in the policy. Globs use two tokens.
//! `*` matches one or more characters of one segment: never a `/`, never
//! nothing. `**` that is a whole segment matches segments: `**/` none or
//! more of them, a closing `/**` all that lies beneath. Any other run of
//! `*` (`src**`, `a/**b`, three or more) is one `*`, as git reads `**`.
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
//! operators put specific rules (`agent:bot-*`) above broad ones (`agent:*`).

use std::fs;
use std::io;
use std::path::Path;

use crate::native::Principal;

/// What a principal may do at the repo level. The fields are *allow* axes —
/// empty lists mean "no constraint on this axis". See the module docs for the
/// surface semantics; the gates are (ref namespace + read-only in
/// `RefStore`, force + path in `NativeRepo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Block every write — refs, objects, and the index.
    pub read_only: bool,
    /// Allow-list for ref names; empty = any ref allowed.
    pub branch_allow: Vec<Glob>,
    /// Deny-list for ref names; takes precedence over allow.
    /// The practical reason this matters even when an allow-list could
    /// theoretically cover the same ground is the "protect main" shape:
    /// `branch=refs/heads/* branch_deny=refs/heads/main` is more
    /// readable than enumerating every other branch. Empty = no deny.
    pub branch_deny: Vec<Glob>,
    /// Allow-list for working-tree paths under `add`/`commit`; empty = any.
    pub path_allow: Vec<Glob>,
    /// Deny non-fast-forward updates and branch deletion.
    pub forbid_force: bool,
    /// when set, the wire receive-pack path rejects any
    /// push that doesn't carry a valid Ed25519 signature over the
    /// canonical push payload. Only enforced on the wire — local CLI
    /// writes don't traverse the signature path.
    pub require_signed: bool,
    /// when set, the wire receive-pack path walks every
    /// newly-pushed commit and rejects the push if any of them lacks a
    /// valid `alt-sig` header from a trusted principal. Composes with
    /// [`require_signed`] (the per-push gate above); both can be on,
    /// either alone, or neither.
    pub require_signed_commits: bool,
}

impl Capabilities {
    /// The zero-regression default: no constraint anywhere. Used when no
    /// policy file exists, or when no rule matches the principal.
    pub fn full() -> Self {
        Self {
            read_only: false,
            branch_allow: Vec::new(),
            branch_deny: Vec::new(),
            path_allow: Vec::new(),
            forbid_force: false,
            require_signed: false,
            require_signed_commits: false,
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
    /// `branch_deny` glob list runs *after* the allow check — a name on the
    /// deny list is refused even when an allow glob accepted it (deny wins).
    pub fn allows_branch_name(&self, name: &str) -> bool {
        if !self.branch_allow.is_empty() && !self.branch_allow.iter().any(|g| g.matches(name)) {
            return false;
        }
        if self.branch_deny.iter().any(|g| g.matches(name)) {
            return false;
        }
        true
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
        } else if tok == "require-signed-commits" {
            caps.require_signed_commits = true;
        } else if let Some(v) = tok.strip_prefix("branch=") {
            caps.branch_allow.push(Glob::new(v));
        } else if let Some(v) = tok.strip_prefix("branch_deny=") {
            caps.branch_deny.push(Glob::new(v));
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

/// A two-token glob as the module docs read it, matched against the whole
/// of a name. Plain text otherwise, no escapes: a `/` in the pattern is a
/// `/` in the name, so `src/**` holds what lies beneath `src` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    pattern: String,
}

impl Glob {
    pub fn new(pat: &str) -> Self {
        Self {
            pattern: pat.to_owned(),
        }
    }

    /// Match the whole `input` against this glob.
    pub fn matches(&self, input: &str) -> bool {
        glob(self.pattern.as_bytes(), input.as_bytes(), true)
    }
}

/// Whether all of `input` is what `pattern` describes; `at_start` where
/// the pattern begins a segment. Patterns are tiny: every length is tried.
fn glob(pattern: &[u8], input: &[u8], at_start: bool) -> bool {
    let Some((&first, rest)) = pattern.split_first() else {
        return input.is_empty();
    };
    if first != b'*' {
        return input.first() == Some(&first) && glob(rest, &input[1..], first == b'/');
    }
    let stars = pattern.iter().take_while(|b| **b == b'*').count();
    let after = &pattern[stars..];
    if stars == 2 && at_start && matches!(after.first(), None | Some(b'/')) {
        let slashes = input.iter().enumerate().filter(|(_, b)| **b == b'/');
        let mut starts = std::iter::once(0).chain(slashes.map(|(at, _)| at + 1));
        return after.is_empty() || starts.any(|at| glob(&after[1..], &input[at..], true));
    }
    let segment = input.iter().take_while(|b| **b != b'/').count();
    (1..=segment).any(|at| glob(after, &input[at..], false))
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
    fn branch_deny_takes_precedence_over_branch_allow() {
        // protect refs/heads/main while allowing every other branch
        let p = Policy::parse("human:alice -> branch=refs/heads/* branch_deny=refs/heads/main")
            .unwrap();
        let caps = p.lookup(&principal(PrincipalKind::Human, "alice"));
        assert!(caps.allows_branch_name("refs/heads/feature-x"));
        assert!(
            !caps.allows_branch_name("refs/heads/main"),
            "deny must win over allow even when allow matches"
        );
    }

    #[test]
    fn branch_deny_without_allow_blocks_only_listed_globs() {
        // no allow-list: every branch is allowed *except* the deny ones
        let p = Policy::parse("human:* -> branch_deny=refs/heads/main").unwrap();
        let caps = p.lookup(&principal(PrincipalKind::Human, "alice"));
        assert!(caps.allows_branch_name("refs/heads/feature-x"));
        assert!(caps.allows_branch_name("refs/heads/release/1.0"));
        assert!(!caps.allows_branch_name("refs/heads/main"));
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
        let g = Glob::new("refs/heads/feature/agent-bot/**");
        assert!(g.matches("refs/heads/feature/agent-bot/x"));
        assert!(g.matches("refs/heads/feature/agent-bot/nested/branch"));
        assert!(g.matches("refs/heads/feature/agent-bot/"));
        assert!(!g.matches("refs/heads/feature/other/x"));

        // `**` between two literals — must still backtrack to find a match
        let g = Glob::new("src/**/lib.rs");
        assert!(g.matches("src/lib.rs"));
        assert!(g.matches("src/a/lib.rs"));
        assert!(g.matches("src/a/b/c/lib.rs"));
        assert!(!g.matches("src/a/lib.rsx"));
    }

    #[test]
    fn a_slash_beside_a_double_star_is_a_slash_of_the_name() {
        let beneath = Glob::new("src/**");
        assert!(beneath.matches("src/lib.rs") && beneath.matches("src/a/b.rs"));
        for beside in ["srcx/y", "srcfile2", "src", "xsrc/y", "a/src/y"] {
            assert!(!beneath.matches(beside), "{beside}");
        }
        let branch = Glob::new("refs/heads/feature/agent-bot/**");
        assert!(!branch.matches("refs/heads/feature/agent-botX/y"));
        assert!(!branch.matches("refs/heads/feature/agent-bot"));
        let anywhere = Glob::new("**/lib.rs");
        assert!(anywhere.matches("lib.rs") && anywhere.matches("a/b/lib.rs"));
        assert!(!anywhere.matches("xlib.rs") && !anywhere.matches("a/xlib.rs"));
        let between = Glob::new("a/**/b");
        assert!(between.matches("a/b") && between.matches("a/x/y/b"));
        assert!(!between.matches("ab") && !between.matches("a/xb") && !between.matches("ax/b"));
        let one = Glob::new("src/*.rs");
        assert!(one.matches("src/a.rs"));
        assert!(!one.matches("src/a/b.rs") && !one.matches("src/.rs") && !one.matches("srcx/a.rs"));
        assert!(Glob::new("**").matches("") && Glob::new("**").matches("a/b"));
        assert!(
            Glob::new("agent:*").matches("agent:bot") && !Glob::new("agent:*").matches("agent:")
        );
    }

    /// Every pattern of up to six of `a`, `*` and `/` against every name of
    /// up to five of `a`, `b` and `/`, held to a matcher that reads the
    /// pattern segment by segment.
    #[test]
    fn every_short_pattern_matches_what_its_segments_say() {
        fn words(letters: &str, longest: usize) -> Vec<String> {
            let mut all = vec![String::new()];
            let mut from = 0;
            for _ in 0..longest {
                let until = all.len();
                for at in from..until {
                    let longer = letters.chars().map(|c| format!("{}{c}", all[at]));
                    all.extend(longer.collect::<Vec<_>>());
                }
                from = until;
            }
            all
        }
        // within a segment a run of stars is one or more characters
        fn segment(pattern: &[u8], name: &[u8]) -> bool {
            match pattern.split_first() {
                None => name.is_empty(),
                Some((b'*', _)) => {
                    let stars = pattern.iter().take_while(|b| **b == b'*').count();
                    (1..=name.len()).any(|at| segment(&pattern[stars..], &name[at..]))
                }
                Some((c, rest)) => name.first() == Some(c) && segment(rest, &name[1..]),
            }
        }
        fn segments(pattern: &[&str], name: &[&str]) -> bool {
            match (pattern.split_first(), name.split_first()) {
                (None, _) => name.is_empty(),
                (Some((&"**", [])), _) => !name.is_empty(),
                (Some((&"**", rest)), _) => (0..=name.len()).any(|at| segments(rest, &name[at..])),
                (Some((first, rest)), Some((n, names))) => {
                    segment(first.as_bytes(), n.as_bytes()) && segments(rest, names)
                }
                (Some(_), None) => false,
            }
        }
        let names = words("ab/", 5);
        let mut matched = 0;
        for pattern in words("a*/", 6) {
            let glob = Glob::new(&pattern);
            let parts: Vec<&str> = pattern.split('/').collect();
            for name in &names {
                let said = segments(&parts, &name.split('/').collect::<Vec<_>>());
                assert_eq!(glob.matches(name), said, "{pattern:?} against {name:?}");
                matched += usize::from(said);
            }
        }
        assert!(matched > 1000, "{matched} matches");
        for (pattern, across) in [
            ("src**", "src/a"),
            ("a**b", "a/b"),
            ("a/**b", "a/x/b"),
            ("a/***/b", "a/x/y/b"),
        ] {
            assert!(!Glob::new(pattern).matches(across), "{pattern}");
        }
        assert!(Glob::new("src**").matches("srcx") && !Glob::new("src**").matches("src"));
        assert!(Glob::new("a/***/b").matches("a/x/b") && !Glob::new("a/***/b").matches("a/b"));
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
        // `agent:bot-*` is specific; `agent:*` is the broad catch-all
        let p = Policy::parse(
            "agent:bot-* -> branch=refs/heads/feature/bot/**\n\
             agent:*        -> read-only\n",
        )
        .unwrap();
        let bot = principal(PrincipalKind::Agent, "bot-opus");
        let caps = p.lookup(&bot);
        assert!(!caps.read_only, "specific rule wins, not the read-only one");
        assert!(caps.allows_branch("refs/heads/feature/bot/x"));
        assert!(!caps.allows_branch("refs/heads/main"));

        let other = principal(PrincipalKind::Agent, "rover");
        assert!(p.lookup(&other).read_only, "catch-all applies to non-bot");

        // Order swap: now the catch-all comes first and shadows the specific
        let p = Policy::parse(
            "agent:*        -> read-only\n\
             agent:bot-* -> branch=refs/heads/feature/bot/**\n",
        )
        .unwrap();
        assert!(
            p.lookup(&bot).read_only,
            "first match wins: catch-all above specific shadows it"
        );
    }

    #[test]
    fn cap_spec_accumulates_branch_and_path() {
        let p =
            Policy::parse("agent:bot -> branch=refs/heads/feature/** path=src/** path=docs/**\n")
                .unwrap();
        let caps = p.lookup(&principal(PrincipalKind::Agent, "bot"));
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
        let tmp = alt_os::temp::tempdir().unwrap();
        let p = Policy::load(tmp.path()).unwrap();
        // empty policy → full capabilities for everyone
        assert_eq!(
            p.lookup(&principal(PrincipalKind::Agent, "x")),
            Capabilities::full()
        );
    }

    #[test]
    fn loads_from_disk() {
        let tmp = alt_os::temp::tempdir().unwrap();
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
