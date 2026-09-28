//! Who is acting: the principal (human or agent) and the author identity
//! commits are made with.

/// What kind of principal is acting: a human user (default) or an automated
/// agent. The op-log records this with the principal's id so a multi-agent
/// workspace can answer "who did this" without losing the human/automation
/// distinction. The capability policy matches on `<kind>:<id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    Human,
    Agent,
}

impl PrincipalKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }
    pub(super) fn parse(s: &str) -> Option<Self> {
        match s {
            "human" => Some(Self::Human),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// The structured operator identity for an op-log entry: kind, stable id, and
/// an optional session correlation token. Encoded into the existing free-form
/// `actor` field on `Op` (wire format unchanged); see [`Principal::actor_string`] and
/// [`Principal::parse_actor`] for the wire format and legacy compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub kind: PrincipalKind,
    pub id: String,
    pub session: Option<String>,
}

impl Principal {
    /// Encode this principal + a verb into the op-log `actor` string. Format:
    /// `<kind>:<id>;session:<s>;user:<u>;verb:<v>`, with `session` omitted when
    /// `None`. `:` and `;` in any value are sanitized to `_` so the grammar is
    /// trivial to re-parse. The `user`
    /// field is carried for debuggability when `id != user` (agent runs as a
    /// human login); [`parse_actor`] keeps it for display only.
    pub fn actor_string(&self, user: &str, verb: &str) -> String {
        let san = |s: &str| s.replace([';', ':'], "_");
        let mut out = format!("{}:{}", self.kind.as_str(), san(&self.id));
        if let Some(s) = &self.session {
            out.push_str(";session:");
            out.push_str(&san(s));
        }
        out.push_str(";user:");
        out.push_str(&san(user));
        out.push_str(";verb:");
        out.push_str(&san(verb));
        out
    }

    /// Inverse of [`actor_string`], plus a compatibility path for the legacy
    /// `cli/<verb>@<user>` form written by older alt — those parse as a Human
    /// principal with `id = user`, no session. Returns `(principal, verb)`;
    /// the verb is the empty string when the input has none.
    pub fn parse_actor(s: &str) -> (Principal, String) {
        if let Some(rest) = s.strip_prefix("cli/")
            && let Some(at) = rest.find('@')
        {
            let verb = rest[..at].to_owned();
            let user = rest[at + 1..].to_owned();
            return (
                Principal {
                    kind: PrincipalKind::Human,
                    id: user,
                    session: None,
                },
                verb,
            );
        }
        let mut parts = s.split(';');
        let head = parts.next().unwrap_or("");
        let (kind_str, id) = head
            .find(':')
            .map(|i| (&head[..i], &head[i + 1..]))
            .unwrap_or(("human", head));
        let mut p = Principal {
            kind: PrincipalKind::parse(kind_str).unwrap_or(PrincipalKind::Human),
            id: id.to_owned(),
            session: None,
        };
        let mut verb = String::new();
        for kv in parts {
            let Some(colon) = kv.find(':') else { continue };
            let (k, v) = (&kv[..colon], &kv[colon + 1..]);
            match k {
                "session" => p.session = Some(v.to_owned()),
                "verb" => verb = v.to_owned(),
                _ => {} // `user:` is informational; future keys parse forward
            }
        }
        (p, verb)
    }
}

/// Who is acting: the structured principal that names the op-log actor and the
/// (separate) author identity for git commits. Built per request from the
/// caller's environment, not from process globals — so the daemon can serve
/// concurrent callers with distinct identities without racing on `std::env`.
///
/// Agent vs human is recorded via [`Principal`] in the op log; commit
/// `author`/`committer` stay human-shaped (`Name <email>`) so export→.git is
/// idiomatic git and external git tools don't see "agent" in author lines.
#[derive(Clone)]
pub struct Identity {
    pub(super) principal: Principal,
    pub(super) user: String,
    pub(super) author_name: String,
    pub(super) author_email: String,
    /// Whom an agent acts for (`ALT_CONTROLLING`), recorded in commit
    /// metadata.
    pub(super) controlling: Option<String>,
}

impl Identity {
    /// From this process's environment (the `alt` CLI path).
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// From a request's forwarded env vars (the daemon path).
    pub fn from_map(env: &[(String, String)]) -> Self {
        Self::from_lookup(|k| {
            env.iter()
                .find(|(name, _)| name == k)
                .map(|(_, v)| v.clone())
        })
    }

    pub(super) fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let user = get("USER").unwrap_or_else(|| "unknown".to_owned());
        let kind = get("ALT_PRINCIPAL_KIND")
            .and_then(|s| PrincipalKind::parse(&s))
            .unwrap_or(PrincipalKind::Human);
        let id = get("ALT_PRINCIPAL_ID").unwrap_or_else(|| user.clone());
        let session = get("ALT_SESSION_ID");
        let author_name = get("GIT_AUTHOR_NAME")
            .or_else(|| get("USER"))
            .unwrap_or_else(|| "alt".to_owned());
        let author_email =
            get("GIT_AUTHOR_EMAIL").unwrap_or_else(|| format!("{author_name}@localhost"));
        Self {
            principal: Principal { kind, id, session },
            user,
            author_name,
            author_email,
            controlling: get("ALT_CONTROLLING").filter(|v| !v.is_empty()),
        }
    }

    /// The op-log actor string for a verb. New structured form via
    /// [`Principal::actor_string`]; the parse side accepts the legacy
    /// `cli/<verb>@<user>` form for ops written by older alt.
    pub(super) fn actor(&self, verb: &str) -> String {
        self.principal.actor_string(&self.user, verb)
    }

    /// The commit author/committer identity (name, email).
    pub(super) fn sig(&self) -> (&str, &str) {
        (&self.author_name, &self.author_email)
    }
}
