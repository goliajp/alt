//! Native `.alt` repository commands: `init`, `add`, `commit`, `status`.
//! These wire the alt-worktree write primitives and alt-refs op log into a
//! dogfoodable commit loop. The control dir is `<root>/.alt`; the index is
//! git index v2 at `.alt/index`; HEAD and branches are native refs.

use std::io::Write;
use std::path::{Path, PathBuf};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_index::{Index, IndexEntry};
use alt_odb::NativeOdb;
use alt_refs::{IdemKey, OpId, RefChange, RefPolicy, RefStore, RefTarget};

use crate::policy::{Capabilities, Policy};
use alt_worktree::{
    ChangeKind, Sig, WorkEntry, build_commit_bytes, flatten_tree, index_entries,
    scan_indexed_paths, scan_worktree, scan_worktree_with_index, status, write_commit, write_tree,
};
use bstr::{BString, ByteSlice};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What kind of principal is acting: a human user (default) or an automated
/// agent. The op-log records this with the principal's id so a multi-agent
/// workspace can answer "who did this" without losing the human/automation
/// distinction. (A5a; A6 will key capabilities off this.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    Human,
    Agent,
}

impl PrincipalKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "human" => Some(Self::Human),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// The structured operator identity for an op-log entry: kind, stable id, and
/// an optional session correlation token. Encoded into the existing free-form
/// `actor` field on `Op` (stone unchanged); see [`Principal::actor_string`] and
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
    /// trivial to re-parse (the only A5 audit consumer right now). The `user`
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
    /// `cli/<verb>@<user>` form written before A5a — those parse as a Human
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
    principal: Principal,
    user: String,
    author_name: String,
    author_email: String,
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

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
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
        }
    }

    /// The op-log actor string for a verb. New structured form via
    /// [`Principal::actor_string`]; the parse side accepts the legacy
    /// `cli/<verb>@<user>` form for ops written before A5a.
    fn actor(&self, verb: &str) -> String {
        self.principal.actor_string(&self.user, verb)
    }

    /// The commit author/committer identity (name, email).
    fn sig(&self) -> (&str, &str) {
        (&self.author_name, &self.author_email)
    }
}

/// The shared, per-repository state under `<root>/.alt`: the object database,
/// the ref store, and the hash algorithm. These are the parts the local daemon
/// holds open across requests (amortizing the per-command open ~21ms); the
/// per-workspace coordinates live in [`NativeRepo`], attached per request.
pub struct Store {
    alt_dir: PathBuf,
    odb: NativeOdb,
    refs: RefStore,
    algo: HashAlgo,
    /// The repository's A6 policy, loaded from `<alt-dir>/policy`. A missing
    /// file → [`Policy::empty`] (every principal gets [`Capabilities::full`]),
    /// the zero-regression default. Daemon re-reads in [`refresh`](Self::refresh)
    /// so an operator edit is picked up on the next request.
    policy: Policy,
}

impl Store {
    /// Opens (does not create) the store whose control dir is `alt_dir`.
    pub fn open(alt_dir: PathBuf) -> Res<Self> {
        Ok(Self {
            odb: NativeOdb::open(&alt_dir)?,
            refs: RefStore::open(&alt_dir)?,
            policy: Policy::load(&alt_dir)?,
            alt_dir,
            algo: HashAlgo::Sha1,
        })
    }

    /// The control directory (`…/.alt`).
    pub fn alt_dir(&self) -> &Path {
        &self.alt_dir
    }

    /// Mutable access to the odb. M9/W10c: `altd-server` puts incoming
    /// pack objects through here directly, since it doesn't own a
    /// NativeRepo (no working tree / index in the wire path).
    pub fn odb_mut(&mut self) -> &mut NativeOdb {
        &mut self.odb
    }

    /// Read-only odb fetch. M10/W15: the wire's commit-signature pass
    /// re-reads each newly-ingested commit through this so verification
    /// shares the same `NativeOdb` everyone else writes against (no
    /// risk of a separate handle pinning a stale view).
    pub fn odb_get(
        &self,
        oid: &ObjectId,
    ) -> Result<Option<alt_git_codec::RawObject>, alt_odb::OdbError> {
        self.odb.get(oid)
    }

    /// Mutable access to the refs store, for the same reason as
    /// [`odb_mut`] — receive-pack commits the pushed ref-update list as
    /// a single transaction through this handle.
    pub fn refs_mut(&mut self) -> &mut RefStore {
        &mut self.refs
    }

    /// Read-path catch-up: bring the odb and ref state up to date with writes
    /// committed by other processes since the last refresh. The daemon calls
    /// this at the start of every request so a served read is never stale.
    /// Also re-loads the A6 policy so a `.alt/policy` edit takes effect on
    /// the next request without restarting the daemon.
    pub fn refresh(&mut self) -> Res<()> {
        self.odb.refresh()?;
        self.refs.refresh()?;
        self.policy = Policy::load(&self.alt_dir)?;
        Ok(())
    }

    /// The capability gate this principal sees for the current policy.
    pub fn capabilities_for(&self, principal: &Principal) -> Capabilities {
        self.policy.lookup(principal)
    }

    /// The principals this store trusts for A5b push signature checks
    /// (M10/W14). Reads `<alt-dir>/trust/<principal>.pub` files via the
    /// same scanner the local sig-verify command uses, so wire and local
    /// trust roots can't diverge.
    pub fn trust_keys(&self) -> Res<Vec<(String, alt_sign::PublicKey)>> {
        read_pubkey_dir(&self.alt_dir.join("trust"))
    }

    /// The op that applied a keyed write, if one is in the durable idempotency
    /// index — i.e. a request carrying `key` already took effect. The daemon
    /// checks this (after [`refresh`](Self::refresh)) before running a keyed
    /// write, so a same-id retry is acked instead of applied twice (D5c). Built
    /// by replay, so it survives a daemon restart.
    pub fn applied_request(&self, key: &IdemKey) -> Option<OpId> {
        self.refs.applied_request(key)
    }

    /// Turns deferred durability on or off across the odb and the ref log. The
    /// daemon turns it on once at open: a write appends under the request lock
    /// but skips its inline fsync; the daemon then fsyncs off the write path via
    /// its group-commit coordinator ([`Store::sink`]), coalescing concurrent
    /// commits onto ~1 fsync. The direct CLI leaves it off (it fsyncs inline).
    pub fn set_defer_durability(&mut self, on: bool) {
        self.odb.set_defer_durability(on);
        self.refs.set_defer_durability(on);
    }

    /// A monotonic count of deferred writes across the odb and ref log. The
    /// daemon snapshots this around a request to tell whether the command wrote
    /// (and so must wait for the group-commit fsync) — so reads (and write-free
    /// commands) never pay a durability round trip.
    pub fn write_epoch(&self) -> u64 {
        self.odb.write_count() + self.refs.write_count()
    }

    /// An independent fsync handle over the odb (chunks → blobmap → `map.alt`)
    /// and the ref log, in that durability order. The daemon holds one and
    /// fsyncs off the write path — it owns its own fds, so a fsync overlaps
    /// concurrent appends and N commits coalesce onto one fsync.
    pub fn sink(&self) -> Res<StoreSink> {
        Ok(StoreSink {
            odb: self.odb.sink()?,
            oplog: self.refs.sync_handle()?,
        })
    }
}

/// Off-write-path durability handle for a [`Store`] (see [`Store::sink`]): the
/// odb (objects) then the oplog (ref ops), so a durable ref op always has its
/// commit object durable. Fsyncs through independent fds, needing no `&mut
/// Store`, so the daemon's group commit overlaps the fsync with appends.
pub struct StoreSink {
    odb: alt_odb::OdbSink,
    oplog: std::fs::File,
}

impl StoreSink {
    /// One fsync of everything currently on disk, in durability order. Because
    /// fsync flushes the whole inode, this makes durable every append present
    /// when it runs — one call covers any number of concurrent commits.
    pub fn fsync_all(&self) -> Res<()> {
        self.odb.fsync()?;
        self.oplog.sync_all()?;
        Ok(())
    }
}

/// The per-workspace coordinates within a repo: the working-tree root, the
/// workspace name, the ref naming its HEAD, and its index file. Resolved per
/// request and cheap to carry by value.
#[derive(Clone)]
pub struct Coord {
    root: PathBuf,
    workspace: String,
    head_ref: String,
    index_path: PathBuf,
}

impl Coord {
    /// The default workspace's coordinates for the repo rooted at `repo_root`
    /// (its HEAD stays the bare `HEAD` ref and its index `.alt/index`, so
    /// existing repos are unchanged).
    fn default_at(repo_root: &Path) -> Coord {
        let alt_dir = repo_root.join(".alt");
        Coord {
            index_path: alt_dir.join("index"),
            root: repo_root.to_path_buf(),
            workspace: DEFAULT_WORKSPACE.to_owned(),
            head_ref: "HEAD".to_owned(),
        }
    }

    /// The coordinates of workspace `name` in the repo whose control dir is
    /// `alt_dir`. The default workspace's working tree is the repo root; a
    /// named one's comes from its registry `meta`.
    fn for_name(alt_dir: &Path, name: &str) -> Res<Coord> {
        let repo_root = alt_dir.parent().unwrap_or(alt_dir);
        if name == DEFAULT_WORKSPACE {
            return Ok(Coord::default_at(repo_root));
        }
        let ws_dir = alt_dir.join("workspaces").join(name);
        let worktree = std::fs::read_to_string(ws_dir.join("meta"))
            .map_err(|_| format!("no such workspace '{name}'"))?;
        Ok(Coord {
            root: PathBuf::from(worktree.trim()),
            workspace: name.to_owned(),
            head_ref: format!("workspaces/{name}/HEAD"),
            index_path: ws_dir.join("index"),
        })
    }
}

/// Walks up from `start` for a directory holding `.alt`, resolving which
/// workspace applies, and returns the control dir plus the coordinates. An
/// explicit `workspace` name always wins. Otherwise the workspace is inferred:
/// under a repo root (a `.alt` directory) → the default workspace; inside a
/// named workspace's working tree (a `.alt` *file* pointing back at the repo,
/// git-worktree style) → that workspace.
pub fn resolve_workspace(start: &Path, workspace: Option<&str>) -> Res<(PathBuf, Coord)> {
    let mut dir: &Path = start;
    loop {
        let marker = dir.join(".alt");
        if marker.is_dir() {
            let coord = Coord::for_name(&marker, workspace.unwrap_or(DEFAULT_WORKSPACE))?;
            return Ok((marker, coord));
        }
        if marker.is_file() {
            let (repo_root, name) = parse_workspace_marker(&marker)?;
            let alt_dir = repo_root.join(".alt");
            let coord = Coord::for_name(&alt_dir, workspace.unwrap_or(&name))?;
            return Ok((alt_dir, coord));
        }
        dir = dir
            .parent()
            .ok_or("not an alt repository (no .alt found)")?;
    }
}

/// Owns an opened store plus the resolved workspace + identity for one CLI
/// invocation. The daemon does not use this — it holds a [`Store`] across
/// requests and attaches a [`NativeRepo`] per request.
pub struct OpenRepo {
    store: Store,
    coord: Coord,
    id: Identity,
}

impl OpenRepo {
    /// Discovers the repo from `start`, selecting the default (or named)
    /// workspace, and opens its store with the given caller identity.
    pub fn discover(start: &Path, workspace: Option<&str>, id: Identity) -> Res<Self> {
        let (alt_dir, coord) = resolve_workspace(start, workspace)?;
        Ok(Self {
            store: Store::open(alt_dir)?,
            coord,
            id,
        })
    }

    /// The bound repo for issuing one command (direct CLI: no idempotency key,
    /// so its writes are plain commits).
    pub fn repo(&mut self) -> NativeRepo<'_> {
        NativeRepo::attach(&mut self.store, self.coord.clone(), self.id.clone(), None)
    }
}

/// `alt init [dir]`: create an empty native repo with an unborn HEAD → main.
/// `alt clone <url> [<dir>]`: init + remote add origin + fetch + switch
/// to the server's HEAD branch (M6/W6). The destination directory defaults
/// to the URL's last path segment with any `.git` suffix stripped — same
/// convention as `git clone`.
///
/// The composition lives at the top level (not on [`NativeRepo`]) because
/// clone starts before any repository exists: it creates the working
/// directory, runs init, then opens the repo to drive fetch/switch
/// in-process.
pub fn clone(
    url: &str,
    dir: Option<&Path>,
    json: bool,
    cwd: &Path,
    id: Identity,
    out: &mut impl Write,
) -> Res<()> {
    let target = match dir {
        Some(d) => d.to_owned(),
        None => {
            let derived = derive_clone_dir(url)
                .ok_or_else(|| format!("cannot derive clone dir from URL '{url}'"))?;
            cwd.join(derived)
        }
    };
    let alt_dir = target.join(".alt");
    if alt_dir.exists() {
        return Err(format!("{} already exists", alt_dir.display()).into());
    }
    if target.exists() {
        let empty = std::fs::read_dir(&target)?.next().is_none();
        if !empty {
            return Err(format!("'{}' exists and is not empty", target.display()).into());
        }
    } else {
        std::fs::create_dir_all(&target)?;
    }

    // init: same control-dir layout as `alt init`, so subsequent commands
    // can discover this clone like any local repo
    init(Some(target.clone()), out)?;

    // everything from here on routes through NativeRepo against the
    // freshly-created store
    let mut open = OpenRepo::discover(&target, None, id)?;
    let mut repo = open.repo();
    repo.remote_add("origin", url, /*json=*/ false, out)?;
    repo.fetch("origin", &[], /*json=*/ false, out)?;

    // pick the local branch to materialise: prefer the remote's HEAD
    // symref target (mirrored as `refs/remotes/origin/HEAD` during fetch),
    // falling back to `main` / `master` / the first remote-tracking head
    let pick = pick_clone_branch(&repo)?;
    if let Some((branch, oid)) = pick {
        let local_ref = format!("refs/heads/{branch}");
        if repo.store_refs_get(&local_ref).is_none() {
            repo.create_branch_at(&local_ref, oid)?;
        }
        // init pointed HEAD at refs/heads/main symbolically; if the
        // remote's default isn't 'main' we need to retarget HEAD too
        let head_target = format!("refs/heads/{branch}");
        let current_head = repo.store_refs_get("HEAD");
        if !matches!(&current_head, Some(RefTarget::Symbolic(s)) if s == &head_target) {
            repo.point_head_at(&head_target)?;
        }
        repo.materialise_head()?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("url", Json::str(url)),
                    ("dir", Json::str(target.display().to_string())),
                    ("branch", Json::str(&branch)),
                    ("head", Json::str(oid.to_string())),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Cloned '{url}' into {} (HEAD → refs/heads/{branch})",
                target.display()
            )?;
        }
    } else if json {
        use crate::json::Json;
        crate::json::emit(
            out,
            vec![
                ("url", Json::str(url)),
                ("dir, no branches", Json::str(target.display().to_string())),
                ("branch", Json::Null),
            ],
        )?;
    } else {
        writeln!(
            out,
            "Cloned '{url}' into {} (remote has no branches — HEAD unset)",
            target.display()
        )?;
    }
    Ok(())
}

fn derive_clone_dir(url: &str) -> Option<String> {
    let last = url.trim_end_matches('/').rsplit('/').next()?;
    let stripped = last.strip_suffix(".git").unwrap_or(last);
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_owned())
    }
}

fn pick_clone_branch(repo: &NativeRepo) -> Res<Option<(String, ObjectId)>> {
    // (1) `refs/remotes/origin/HEAD` symref written by our fetch path
    if let Some(RefTarget::Symbolic(target)) = repo.store_refs_get("refs/remotes/origin/HEAD")
        && let Some(branch) = target.strip_prefix("refs/remotes/origin/")
        && let Some(RefTarget::Oid(oid)) = repo.store_refs_get(&target)
    {
        return Ok(Some((branch.to_owned(), oid)));
    }
    // (2) common defaults
    for cand in ["main", "master"] {
        let r = format!("refs/remotes/origin/{cand}");
        if let Some(RefTarget::Oid(oid)) = repo.store_refs_get(&r) {
            return Ok(Some((cand.to_owned(), oid)));
        }
    }
    // (3) first remote-tracking branch in name order
    for (name, target) in repo.store_refs_iter() {
        if let Some(branch) = name.strip_prefix("refs/remotes/origin/")
            && branch != "HEAD"
            && let RefTarget::Oid(oid) = target
        {
            return Ok(Some((branch.to_owned(), oid)));
        }
    }
    Ok(None)
}

pub fn init(dir: Option<PathBuf>, out: &mut impl Write) -> Res<()> {
    let root = dir.unwrap_or_else(|| PathBuf::from("."));
    let alt_dir = root.join(".alt");
    if alt_dir.exists() {
        return Err(format!("{} already exists", alt_dir.display()).into());
    }
    std::fs::create_dir_all(&root)?;
    let mut odb = NativeOdb::open(&alt_dir)?;
    odb.flush()?;
    let mut refs = RefStore::open(&alt_dir)?;
    refs.commit(
        &Identity::from_env().actor("init"),
        now_ms(),
        &[RefChange {
            name: "HEAD".to_owned(),
            old: None,
            new: Some(RefTarget::Symbolic("refs/heads/main".to_owned())),
        }],
    )?;
    save_index(&alt_dir.join("index"), &empty_index())?;
    writeln!(
        out,
        "Initialized empty alt repository in {}",
        alt_dir.display()
    )?;
    Ok(())
}

/// The default workspace's name — its HEAD stays the bare `HEAD` ref and its
/// index stays at `.alt/index`, so existing repos are unchanged.
const DEFAULT_WORKSPACE: &str = "default";

/// An opened native repo bound to one workspace, for the span of one command.
/// It borrows the shared [`Store`] (so the daemon can reuse one across requests)
/// and owns the per-request coordinates and caller identity. The odb, branch
/// refs, and op log are shared across workspaces; the HEAD ref, index, and
/// working tree are per-workspace, so N agents can work in parallel.
pub struct NativeRepo<'a> {
    store: &'a mut Store,
    root: PathBuf,
    /// This workspace's name (`default` for the repo-root workspace).
    workspace: String,
    /// The ref naming this workspace's HEAD: `HEAD` for the default workspace,
    /// `workspaces/<name>/HEAD` for a named one (kept out of `refs/heads/`).
    head_ref: String,
    /// This workspace's index file.
    index_path: PathBuf,
    /// The caller acting through this view.
    id: Identity,
    /// The A6 capability gate for this caller's [`Principal`], derived from
    /// the store's policy at [`attach`](Self::attach). Force / path gates are
    /// applied here in `NativeRepo`; ref-namespace / read-only gates are
    /// applied inside [`RefStore::commit_idempotent`] via [`RefPolicy`].
    caps: Capabilities,
    /// The idempotency key stamped on this command's terminal ref transaction
    /// (D5c). `Some` only on the daemon's exactly-once write path; `None` for
    /// reads and the direct CLI (where `commit_idempotent` is a plain commit).
    idem_key: Option<IdemKey>,
}

impl<'a> NativeRepo<'a> {
    /// Binds a borrowed store to a workspace + identity for one command, with an
    /// optional idempotency `idem_key` (the daemon injects it for a keyed write;
    /// `None` everywhere else).
    pub fn attach(
        store: &'a mut Store,
        coord: Coord,
        id: Identity,
        idem_key: Option<IdemKey>,
    ) -> Self {
        let caps = store.capabilities_for(&id.principal);
        Self {
            store,
            root: coord.root,
            workspace: coord.workspace,
            head_ref: coord.head_ref,
            index_path: coord.index_path,
            id,
            caps,
            idem_key,
        }
    }

    /// Reject the operation when the caller's policy forbids any write. Called
    /// at the top of every mutating command — covers `add` (which only writes
    /// the index, not refs) as well as the ref-producing commands.
    /// W8 — local A6 branch_allow check for a push. `branch_allow` is a
    /// list of glob patterns; if non-empty, every remote ref a push would
    /// touch must match at least one pattern. The check uses the *short*
    /// branch name (the part after `refs/heads/`) so a pattern like
    /// `feature/*` lines up with how A6 patterns are written for local
    /// commit gating.
    fn ensure_push_branch_allowed(&self, changes: &[RefChange]) -> Res<()> {
        let allow = &self.caps.branch_allow;
        if allow.is_empty() {
            return Ok(());
        }
        for c in changes {
            let short = match c.name.strip_prefix("refs/heads/") {
                Some(s) => s,
                // non-branch updates (tag pushes, etc.) fall outside the
                // branch_allow gate — those would land in a separate
                // ref_allow when we grow one
                None => continue,
            };
            if !allow.iter().any(|g| g.matches(short)) {
                return Err(format!(
                    "capability denied: push to '{}' is not in branch_allow",
                    c.name
                )
                .into());
            }
        }
        Ok(())
    }

    /// W8 — git-default "non-fast-forward needs `-f`" gate, independent
    /// of A6. Force (`-f`) skips this; A6's `forbid_force` is the deeper
    /// gate that even `-f` cannot bypass (handled separately by
    /// [`ensure_no_force`]).
    fn ensure_fast_forward(&self, changes: &[RefChange]) -> Res<()> {
        for c in changes {
            let nff = match (&c.old, &c.new) {
                // delete a ref that exists on the remote
                (Some(_), None) => true,
                // create / symref — nothing on the remote to overwrite
                (None, _) => false,
                (Some(RefTarget::Symbolic(_)), _) | (_, Some(RefTarget::Symbolic(_))) => false,
                (Some(RefTarget::Oid(old)), Some(RefTarget::Oid(new))) => {
                    // The remote-side `old` must be present locally for an
                    // ancestor check; if it isn't, the user needs to fetch
                    // first — surface that as a clear precondition error.
                    match self.try_is_ancestor(*old, *new)? {
                        Some(is_anc) => !is_anc,
                        None => {
                            return Err(format!(
                                "remote ref '{}' is at {old} which is not in the local odb; \
                                 run `alt fetch <remote>` and re-try",
                                c.name
                            )
                            .into());
                        }
                    }
                }
            };
            if nff {
                return Err(
                    format!("non-fast-forward push to '{}'; pass -f to override", c.name).into(),
                );
            }
        }
        Ok(())
    }

    /// Like [`is_ancestor`] but `Ok(None)` when one of the commits is
    /// missing from the local odb instead of erroring — lets the caller
    /// surface a precise "fetch first" message for the push pre-check.
    fn try_is_ancestor(&self, old: ObjectId, new: ObjectId) -> Res<Option<bool>> {
        if old == new {
            return Ok(Some(true));
        }
        let repo = alt_repo::Repository::discover(&self.store.alt_dir)?;
        if repo.read_object(&old)?.is_none() || repo.read_object(&new)?.is_none() {
            return Ok(None);
        }
        Ok(Some(self.is_ancestor(old, new)?))
    }

    fn ensure_writable(&self, verb: &str) -> Res<()> {
        if self.caps.read_only {
            return Err(format!("capability denied: principal cannot {verb} (read-only)").into());
        }
        Ok(())
    }

    /// Reject staging/committing a path that the policy's `path_allow` does
    /// not match. Empty allow-list = no constraint.
    fn ensure_path_allowed(&self, path: &str) -> Res<()> {
        if !self.caps.allows_path_name(path) {
            return Err(format!("capability denied: path '{path}' is not in path_allow").into());
        }
        Ok(())
    }

    /// Reject a ref change that would be a non-fast-forward (or branch
    /// deletion) when the policy sets `forbid_force`. A new ref pointing at a
    /// commit that is *not* a descendant of the old one (or `new = None` for
    /// deletion) is force. Creations (`old = None`) and symref moves are
    /// allowed: nothing of value is being overwritten.
    fn ensure_no_force(&self, changes: &[RefChange]) -> Res<()> {
        if !self.caps.forbid_force {
            return Ok(());
        }
        for c in changes {
            let force = match (&c.old, &c.new) {
                // deletion is the canonical force op
                (Some(_), None) => true,
                // creation cannot rewrite history
                (None, _) => false,
                // symref move: the source/destination are names, not commits;
                // no commit graph is being overwritten
                (Some(RefTarget::Symbolic(_)), _) | (_, Some(RefTarget::Symbolic(_))) => false,
                (Some(RefTarget::Oid(old)), Some(RefTarget::Oid(new))) => {
                    !self.is_ancestor(*old, *new)?
                }
            };
            if force {
                return Err(format!(
                    "capability denied: forbid_force is set and {} would not be a fast-forward",
                    c.name
                )
                .into());
            }
        }
        Ok(())
    }

    /// The single gate every CLI ref-changing op must go through: enforces
    /// read-only and force/non-FF *before* entering the ref store, then asks
    /// the store to do the atomic CAS-append with the namespace gate folded
    /// in (via [`RefPolicy`]). A denial — at any axis — costs no on-disk
    /// state. Uses [`self.idem_key`](Self::idem_key) so daemon-keyed writes
    /// flow through unchanged (D5c). Use [`commit_refs_unkeyed`] for the
    /// *non-terminal* tx of a multi-tx command (e.g. switch-with-create).
    fn commit_refs(&mut self, verb: &str, changes: &[RefChange]) -> Res<OpId> {
        self.commit_refs_with(verb, changes, self.idem_key)
    }

    /// Like [`commit_refs`] but always with `idem_key = None` — for the
    /// non-terminal ref tx of a command that produces several (switch-with-
    /// create's create-branch, workspace add's HEAD wire-up). The terminal
    /// tx of the same command goes through [`commit_refs`] so D5c's
    /// exactly-once retry still works.
    fn commit_refs_unkeyed(&mut self, verb: &str, changes: &[RefChange]) -> Res<OpId> {
        self.commit_refs_with(verb, changes, None)
    }

    fn commit_refs_with(
        &mut self,
        verb: &str,
        changes: &[RefChange],
        key: Option<IdemKey>,
    ) -> Res<OpId> {
        self.ensure_writable(verb)?;
        self.ensure_no_force(changes)?;
        let allow = self.caps.branch_allow.clone();
        let read_only = self.caps.read_only;
        let has_allow = !allow.is_empty();
        let is_branch_allowed = move |name: &str| allow.iter().any(|g| g.matches(name));
        let policy = RefPolicy {
            read_only,
            is_branch_allowed: if has_allow {
                Some(&is_branch_allowed)
            } else {
                None
            },
        };
        let actor = self.id.actor(verb);
        let id =
            self.store
                .refs
                .commit_idempotent(&actor, now_ms(), changes, key, Some(&policy))?;
        // A5b: opt-in op-level signing. Best-effort — a missing sec key
        // logs nothing and is not fatal (signing is a per-repo capability,
        // not a hard requirement). Verification at read time tells the
        // auditor which ops are signed and which aren't.
        self.maybe_sign_op(id)?;
        Ok(id)
    }

    /// Look up the active signing policy and, when enabled and the
    /// caller has a sec key on disk, append a sidecar signature record
    /// for `op_id` to `<alt-dir>/oplog/sigs.log`. Returns Ok even when
    /// policy is off — signing is opt-in.
    fn maybe_sign_op(&self, op_id: OpId) -> Res<()> {
        let policy = SignPolicy::load(&self.store.alt_dir)?;
        if !policy.enabled {
            return Ok(());
        }
        let principal = policy
            .principal
            .unwrap_or_else(|| self.id.principal.id.clone());
        let sec_path = self
            .store
            .alt_dir
            .join("identity")
            .join(format!("{principal}.sec"));
        let sec_text = match std::fs::read_to_string(&sec_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let sec = alt_sign::SecretKey::from_text(&sec_text)
            .map_err(|e| format!("malformed sec key at {}: {e}", sec_path.display()))?;
        let sig = sec.sign(&op_id.0);
        let line = format!("{op_id} {principal} {}", sig.to_text());
        // sigs.log is append-only, line-oriented; one open per write is
        // simpler than holding a handle and matches op-log's own cadence
        // (write transactions are infrequent compared to reads)
        let sigs_path = self.store.alt_dir.join("oplog").join("sigs.log");
        std::fs::create_dir_all(sigs_path.parent().unwrap())?;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&sigs_path)?;
        f.write_all(line.as_bytes())?;
        Ok(())
    }

    /// W9 — when signing is enabled, return `Some((principal, sig_text))`
    /// for the canonical push payload over `updates`; the caller appends
    /// the pair to the wire capability list. Returns `Ok(None)` when
    /// signing is off or the sec key isn't present (a sign-policy file
    /// pointing at an absent principal isn't fatal — push still goes
    /// out, just unsigned, same fall-through as `maybe_sign_op`).
    fn maybe_sign_push(&self, updates: &[alt_wire::RefUpdate]) -> Res<Option<(String, String)>> {
        let policy = SignPolicy::load(&self.store.alt_dir)?;
        if !policy.enabled {
            return Ok(None);
        }
        let principal = policy
            .principal
            .unwrap_or_else(|| self.id.principal.id.clone());
        let sec_path = self
            .store
            .alt_dir
            .join("identity")
            .join(format!("{principal}.sec"));
        let sec_text = match std::fs::read_to_string(&sec_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let sec = alt_sign::SecretKey::from_text(&sec_text)
            .map_err(|e| format!("malformed sec key at {}: {e}", sec_path.display()))?;
        let payload = alt_wire::canonical_push_payload(updates, self.store.algo);
        let sig = sec.sign(&payload);
        // .to_text() includes a trailing newline; the wire cap list can
        // not carry one, so strip it before declaring the cap
        let sig_text = sig.to_text().trim().to_owned();
        Ok(Some((principal, sig_text)))
    }

    /// M10/W15 — when sign-policy is on and a sec key is on disk,
    /// produce the *signed* form of `unsigned_bytes` (an `alt-sig`
    /// header line spliced into the commit's header block). The caller
    /// rehashes and puts to the odb. `Ok(None)` means "leave the commit
    /// unsigned" — same fall-through as [`maybe_sign_push`].
    fn maybe_sign_commit_bytes(&self, unsigned_bytes: &[u8]) -> Res<Option<Vec<u8>>> {
        let policy = SignPolicy::load(&self.store.alt_dir)?;
        if !policy.enabled {
            return Ok(None);
        }
        let principal = policy
            .principal
            .unwrap_or_else(|| self.id.principal.id.clone());
        let sec_path = self
            .store
            .alt_dir
            .join("identity")
            .join(format!("{principal}.sec"));
        let sec_text = match std::fs::read_to_string(&sec_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let sec = alt_sign::SecretKey::from_text(&sec_text)
            .map_err(|e| format!("malformed sec key at {}: {e}", sec_path.display()))?;
        let sig = sec.sign(unsigned_bytes);
        let line = crate::commit_sign::alt_sig_line(&principal, &sig);
        let signed = crate::commit_sign::embed_alt_sig(unsigned_bytes, &line)
            .ok_or("commit bytes have no header/body separator")?;
        Ok(Some(signed))
    }

    /// Walks `new`'s ancestor chain to see whether `old` appears (using the
    /// repository's git object graph via the open `Repository`). Used by
    /// [`ensure_no_force`] for the non-fast-forward check.
    fn is_ancestor(&self, old: ObjectId, new: ObjectId) -> Res<bool> {
        if old == new {
            return Ok(true);
        }
        let repo = alt_repo::Repository::discover(&self.store.alt_dir)?;
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![new];
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            if c == old {
                return Ok(true);
            }
            let obj = repo
                .read_object(&c)?
                .ok_or_else(|| format!("missing commit {c}"))?;
            let commit = alt_git_codec::Commit::parse(&obj.data)?;
            for p in commit.parents() {
                stack.push(p);
            }
        }
        Ok(false)
    }

    /// `alt workspace add <name> <path>`: create a parallel workspace whose
    /// working tree is `worktree`, checked out on `branch`. The HEAD is a
    /// per-workspace ref in the shared store, so it is transactional and
    /// undoable; the index and working tree are this workspace's alone.
    pub fn create_workspace(&mut self, name: &str, worktree: &Path, branch: &str) -> Res<()> {
        check_workspace_name(name)?;
        let head_ref = format!("workspaces/{name}/HEAD");
        if self.store.refs.get(&head_ref).is_some() {
            return Err(format!("a workspace named '{name}' already exists").into());
        }
        let branch_ref = format!("refs/heads/{branch}");
        let commit = self
            .store
            .refs
            .resolve(&branch_ref)?
            .ok_or_else(|| format!("invalid reference: {branch}"))?;

        // the working tree must live outside the repository: a tree nested
        // under another workspace's would show up there as untracked files.
        std::fs::create_dir_all(worktree)?;
        let abs = std::fs::canonicalize(worktree)?;
        let repo_root =
            std::fs::canonicalize(self.store.alt_dir.parent().unwrap_or(&self.store.alt_dir))?;
        if abs.starts_with(&repo_root) {
            return Err("workspace working tree must be outside the repository".into());
        }

        let ws_dir = self.store.alt_dir.join("workspaces").join(name);
        std::fs::create_dir_all(&ws_dir)?;
        std::fs::write(
            ws_dir.join("meta"),
            abs.to_str().ok_or("non-utf8 worktree path")?,
        )?;
        // a `.alt` *file* in the working tree points back at the repo, so
        // commands run from inside it auto-select this workspace (git-worktree
        // style). scan_worktree skips `.alt` by name, so it is not content.
        std::fs::write(
            abs.join(".alt"),
            format!(
                "{}\n{name}\n",
                repo_root.to_str().ok_or("non-utf8 repo path")?
            ),
        )?;

        // point this workspace's HEAD at the branch (one ref transaction)
        self.commit_refs_unkeyed(
            "workspace",
            &[RefChange {
                name: head_ref.clone(),
                old: None,
                new: Some(RefTarget::Symbolic(branch_ref)),
            }],
        )?;

        // materialize the branch tree into the new working tree + index by
        // attaching a child view (sharing this store) and checking out from an
        // empty base
        let child = Coord {
            root: abs,
            workspace: name.to_owned(),
            head_ref,
            index_path: ws_dir.join("index"),
        };
        let mut ws = NativeRepo::attach(&mut *self.store, child, self.id.clone(), None);
        let target = ws.commit_entries(commit)?;
        ws.checkout(&[], &target)?;
        Ok(())
    }

    /// `alt workspace remove <name>`: drop a named workspace's HEAD ref and
    /// control dir. The working-tree files are left in place (the caller owns
    /// them); the default workspace cannot be removed.
    pub fn remove_workspace(&mut self, name: &str) -> Res<()> {
        if name == DEFAULT_WORKSPACE {
            return Err("cannot remove the default workspace".into());
        }
        let head_ref = format!("workspaces/{name}/HEAD");
        let old = self
            .store
            .refs
            .get(&head_ref)
            .cloned()
            .ok_or_else(|| format!("no such workspace '{name}'"))?;
        self.commit_refs_unkeyed(
            "workspace",
            &[RefChange {
                name: head_ref,
                old: Some(old),
                new: None,
            }],
        )?;
        let ws_dir = self.store.alt_dir.join("workspaces").join(name);
        // drop the working tree's `.alt` marker so it no longer resolves
        if let Ok(worktree) = std::fs::read_to_string(ws_dir.join("meta")) {
            let _ = std::fs::remove_file(PathBuf::from(worktree.trim()).join(".alt"));
        }
        if ws_dir.exists() {
            std::fs::remove_dir_all(&ws_dir)?;
        }
        Ok(())
    }

    /// All workspaces: the default plus every registered named one, as
    /// `(name, working-tree path, is-current)`.
    pub fn list_workspaces(&self) -> Res<Vec<(String, PathBuf, bool)>> {
        let mut out = vec![(
            DEFAULT_WORKSPACE.to_owned(),
            self.store
                .alt_dir
                .parent()
                .unwrap_or(&self.store.alt_dir)
                .to_path_buf(),
            self.workspace == DEFAULT_WORKSPACE,
        )];
        let ws_root = self.store.alt_dir.join("workspaces");
        if let Ok(entries) = std::fs::read_dir(&ws_root) {
            let mut named: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            named.sort_by_key(|e| e.file_name());
            for entry in named {
                let name = entry.file_name().to_string_lossy().into_owned();
                let meta = entry.path().join("meta");
                if let Ok(worktree) = std::fs::read_to_string(&meta) {
                    out.push((
                        name.clone(),
                        PathBuf::from(worktree.trim()),
                        self.workspace == name,
                    ));
                }
            }
        }
        Ok(out)
    }

    /// `alt workspace add <name> <path> [branch]`: create the workspace (on
    /// `branch`, defaulting to the current branch) and report it.
    pub fn workspace_add(
        &mut self,
        name: &str,
        path: &Path,
        branch: Option<&str>,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let cur = self.head_branch()?;
        let branch = match branch {
            Some(b) => b.to_owned(),
            None => cur.strip_prefix("refs/heads/").unwrap_or(&cur).to_owned(),
        };
        self.create_workspace(name, path, &branch)?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("workspace", Json::str(name)),
                    ("branch", Json::str(&branch)),
                    ("path", Json::str(path.to_string_lossy().as_bytes())),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Created workspace '{name}' at {} on {branch}",
                path.display()
            )?;
        }
        Ok(())
    }

    /// `alt workspace remove <name>`: drop the workspace and report it.
    pub fn workspace_remove(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        self.remove_workspace(name)?;
        if json {
            use crate::json::Json;
            crate::json::emit(out, vec![("removed", Json::str(name))])?;
        } else {
            writeln!(out, "Removed workspace '{name}'")?;
        }
        Ok(())
    }

    /// `alt workspace list`: the workspaces, human or JSON.
    pub fn workspace_list(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let list = self.list_workspaces()?;
        if json {
            use crate::json::Json;
            let arr = list
                .iter()
                .map(|(name, path, current)| {
                    Json::Object(vec![
                        ("name", Json::str(name)),
                        ("path", Json::str(path.to_string_lossy().as_bytes())),
                        ("current", Json::Bool(*current)),
                    ])
                })
                .collect();
            crate::json::emit(out, vec![("workspaces", Json::Array(arr))])?;
        } else {
            for (name, path, current) in &list {
                let mark = if *current { "* " } else { "  " };
                writeln!(out, "{mark}{name}\t{}", path.display())?;
            }
        }
        Ok(())
    }

    fn index(&self) -> Res<Index> {
        match Index::open(&self.index_path, self.store.algo) {
            Ok(i) => Ok(i),
            Err(alt_git_index::IndexError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(empty_index())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// `alt add <paths>`: stage the given paths (or everything for `.`),
    /// updating the index to match the working tree.
    pub fn add(&mut self, paths: &[String], json: bool, out: &mut impl Write) -> Res<()> {
        self.ensure_writable("add")?;
        // add re-reads + re-puts every staged path anyway; the stat-cache
        // fast path would save the scan-time hash but lose to read_for +
        // odb.put on the next line. Wins for a smarter add are tracked
        // separately (the same-oid short-circuit in odb.put helps a bit).
        let scan = scan_worktree(&self.root, self.store.algo)?;
        let staging_all = paths.iter().any(|p| p == ".");

        // snapshot the prior stage-0 entries — both the starting point for
        // the new index and the "old" side of every IndexChange we'll
        // record in the op log so `alt undo` can roll a stray `add` back.
        let prior_stage_zero: Vec<IndexEntry> = self
            .index()?
            .entries
            .into_iter()
            .filter(|e| e.stage() == 0)
            .collect();
        let mut entries: Vec<IndexEntry> = if staging_all {
            Vec::new()
        } else {
            prior_stage_zero.clone()
        };

        let mut staged = 0;
        let targets: Vec<BString> = if staging_all {
            scan.iter().map(|w| w.path.clone()).collect()
        } else {
            paths.iter().map(|p| BString::from(p.as_str())).collect()
        };
        // Touched paths are the union of "what changed in `entries`" — for
        // staging_all it's the whole prior + everything in scan; for the
        // explicit path form it's just the listed targets. We compute the
        // change list after the index is rewritten so old/new come from
        // the actual entries the index will hold.
        let touched_paths: std::collections::BTreeSet<BString> = if staging_all {
            prior_stage_zero
                .iter()
                .map(|e| e.path.clone())
                .chain(scan.iter().map(|w| w.path.clone()))
                .collect()
        } else {
            targets.iter().cloned().collect()
        };
        for rel in &targets {
            entries.retain(|e| &e.path != rel);
            if let Some(w) = scan.iter().find(|w| &w.path == rel) {
                // A6 path gate: deny staging any path the policy excludes,
                // *before* writing the blob into the odb — so a denial costs
                // no on-disk side effect (the odb put would otherwise persist
                // before the index is even written).
                let path_str = w.path.to_str().unwrap_or("");
                self.ensure_path_allowed(path_str)?;
                self.store
                    .odb
                    .put(w.oid, ObjectKind::Blob, &self.read_for(w)?)?;
                entries.push(self.make_entry(w)?);
                staged += 1;
            } // a path that vanished from the tree is dropped (staged deletion)
        }

        self.store.odb.flush()?;
        save_index(
            &self.index_path,
            &Index {
                version: 2,
                entries: entries.clone(),
                extensions: Vec::new(),
            },
        )?;

        // M8-B1: record the index delta so `alt undo` can roll an `add`
        // back. Skip when the call was a true no-op (touched zero paths or
        // every touched path's entry was unchanged) — keeps an empty add
        // from polluting the op log and wasting an undo step.
        let prior_by_path: std::collections::HashMap<&BString, &IndexEntry> =
            prior_stage_zero.iter().map(|e| (&e.path, e)).collect();
        let new_by_path: std::collections::HashMap<&BString, &IndexEntry> =
            entries.iter().map(|e| (&e.path, e)).collect();
        let mut changes = Vec::new();
        for p in &touched_paths {
            let old = prior_by_path.get(p).map(|e| (e.oid, e.mode));
            let new = new_by_path.get(p).map(|e| (e.oid, e.mode));
            if old != new {
                changes.push(crate::index_tx::IndexChange {
                    path: p.clone(),
                    old,
                    new,
                });
            }
        }
        if !changes.is_empty() {
            let payload = crate::index_tx::encode(&changes, self.store.algo);
            let actor = self.id.actor("add");
            self.store.refs.record_op(&actor, now_ms(), &payload)?;
        }
        if json {
            crate::json::emit(out, vec![("staged", crate::json::Json::Num(staged as i64))])?;
        } else {
            writeln!(out, "staged {staged} path(s)")?;
        }
        Ok(())
    }

    fn read_for(&self, w: &WorkEntry) -> Res<Vec<u8>> {
        let abs = self
            .root
            .join(w.path.to_path().map_err(|_| "non-utf8 path")?);
        if w.mode == 0o120000 {
            Ok(std::fs::read_link(&abs)?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec())
        } else {
            Ok(std::fs::read(&abs)?)
        }
    }

    fn make_entry(&self, w: &WorkEntry) -> Res<IndexEntry> {
        // Gitlinks (submodules) have no on-disk file at the path — only an
        // empty placeholder directory (or nothing, when the submodule has
        // not been initialised). Git encodes their index entry with zero
        // stat fields; do the same so the index round-trips cleanly.
        if w.mode == 0o160000 {
            return Ok(gitlink_entry(w));
        }
        let abs = self
            .root
            .join(w.path.to_path().map_err(|_| "non-utf8 path")?);
        let meta = std::fs::symlink_metadata(&abs)?;
        Ok(stat_entry(&meta, w))
    }

    /// `alt commit -m <msg>`: write a tree + commit from the index, advance
    /// the current branch in one ref transaction.
    pub fn commit(&mut self, message: &str, json: bool, out: &mut impl Write) -> Res<()> {
        self.ensure_writable("commit")?;
        let index = self.index()?;
        let staged = index_entries(&index);
        if staged.is_empty() {
            return Err("nothing to commit (empty index)".into());
        }
        // Path gate is `add`-only on purpose: the restricted principal's
        // *choice* of what to stage is what the policy constrains. Pre-existing
        // index entries inherited from another principal (e.g. operator's
        // baseline) ride through to the commit unchallenged — penalising the
        // agent for paths it never touched is unhelpful.
        let tree = write_tree(&mut self.store.odb, &staged, self.store.algo)?;

        let branch = self.head_branch()?;
        let parent = self.store.refs.resolve(&branch)?;
        let parents: Vec<ObjectId> = parent.into_iter().collect();

        let when = (now_ms() / 1000) as i64;
        let (name, email) = self.id.sig();
        let sig = Sig {
            name,
            email,
            when,
            tz: "+0000",
        };
        let msg = if message.ends_with('\n') {
            message.to_owned()
        } else {
            format!("{message}\n")
        };
        let mut bytes = build_commit_bytes(tree, &parents, &sig, &sig, &msg);
        // M10/W15: when sign-policy is on and a sec key is on disk for
        // the principal, splice an `alt-sig` header into the commit and
        // rehash. The signed commit is the canonical commit from the
        // store's POV — there is no second "unsigned" stored.
        if let Some(signed) = self.maybe_sign_commit_bytes(&bytes)? {
            bytes = signed;
        }
        let commit = ObjectId::hash_object(self.store.algo, ObjectKind::Commit, &bytes);
        self.store.odb.put(commit, ObjectKind::Commit, &bytes)?;
        self.store.odb.flush()?;

        self.commit_refs(
            "commit",
            &[RefChange {
                name: branch.clone(),
                old: parent.map(RefTarget::Oid),
                new: Some(RefTarget::Oid(commit)),
            }],
        )?;
        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("branch", Json::str(short)),
                    ("commit", Json::str(commit.to_string())),
                    ("tree", Json::str(tree.to_string())),
                ],
            )?;
        } else {
            writeln!(out, "[{short}] {commit}")?;
        }
        Ok(())
    }

    /// `alt status`: staged / unstaged / untracked against HEAD and the index,
    /// plus any unmerged (conflicted) paths left by a merge. With `json`, emits
    /// the stable structured schema instead of the human view (VISION §4 A1).
    pub fn status(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let branch = self.head_branch()?;
        let head = self.head_entries()?;
        let raw = self.index()?;
        let unmerged: std::collections::BTreeSet<BString> = raw
            .entries
            .iter()
            .filter(|e| e.stage() > 0)
            .map(|e| e.path.clone())
            .collect();
        let index = index_entries(&raw);
        // Stat cache fast path: skip the read+hash on any file whose
        // mtime/ctime/size/dev/ino/mode still matches the index entry —
        // git's classic optimisation; on a 50k-file monorepo clean run
        // this drops `alt status` from seconds to milliseconds.
        let worktree = scan_worktree_with_index(&self.root, &raw, self.store.algo)?;
        let mut st = status(&head, &index, &worktree);
        // unmerged paths are reported in their own section, not as
        // staged/unstaged/untracked noise driven by the missing stage-0 entry
        st.staged.retain(|(p, _)| !unmerged.contains(p));
        st.unstaged.retain(|(p, _)| !unmerged.contains(p));
        st.untracked.retain(|p| !unmerged.contains(p));

        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        if json {
            return render_status_json(out, short, &self.id.principal, &st, &unmerged);
        }
        writeln!(out, "On branch {short}")?;
        let mark = |k: ChangeKind| match k {
            ChangeKind::Added => "new file",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
        };
        if !unmerged.is_empty() {
            writeln!(out, "Unmerged paths:")?;
            for p in &unmerged {
                writeln!(out, "\tboth modified:   {p}")?;
            }
        }
        if !st.staged.is_empty() {
            writeln!(out, "Changes to be committed:")?;
            for (p, k) in &st.staged {
                writeln!(out, "\t{}:   {p}", mark(*k))?;
            }
        }
        if !st.unstaged.is_empty() {
            writeln!(out, "Changes not staged for commit:")?;
            for (p, k) in &st.unstaged {
                writeln!(out, "\t{}:   {p}", mark(*k))?;
            }
        }
        if !st.untracked.is_empty() {
            writeln!(out, "Untracked files:")?;
            for p in &st.untracked {
                writeln!(out, "\t{p}")?;
            }
        }
        if unmerged.is_empty()
            && st.staged.is_empty()
            && st.unstaged.is_empty()
            && st.untracked.is_empty()
        {
            writeln!(out, "nothing to commit, working tree clean")?;
        }
        Ok(())
    }

    /// The HEAD commit's tree flattened to entries (empty when unborn).
    fn head_entries(&self) -> Res<Vec<WorkEntry>> {
        match self.store.refs.resolve(&self.head_branch()?)? {
            Some(commit) => self.commit_entries(commit),
            None => Ok(Vec::new()),
        }
    }

    /// A commit's tree flattened to path-sorted entries.
    fn commit_entries(&self, commit: ObjectId) -> Res<Vec<WorkEntry>> {
        let obj = self
            .store
            .odb
            .get(&commit)?
            .ok_or("commit missing from store")?;
        let tree = alt_git_codec::Commit::parse(&obj.data)?
            .tree()
            .ok_or("commit has no tree")?;
        Ok(flatten_tree(&self.store.odb, tree, self.store.algo)?)
    }

    /// `alt diff` (index → working tree) or `alt diff --cached` (HEAD →
    /// index): a git-style unified diff of the tracked changes. With `json`,
    /// emits the structured per-file/per-hunk schema (VISION §4 A1). With
    /// `semantic`, item-level AST diff (A8b) replaces the unified diff for
    /// files in a language we have a parser for (`.rs` today); other files
    /// fall back to the same line/binary output as without `--semantic`.
    pub fn diff(&self, cached: bool, json: bool, semantic: bool, out: &mut impl Write) -> Res<()> {
        // old side is always read from the object store; the new side is the
        // index (cached) or the live working tree (default).
        let (old, new, new_on_disk, show_added) = if cached {
            (
                self.head_entries()?,
                index_entries(&self.index()?),
                false,
                true,
            )
        } else {
            let raw = self.index()?;
            // Sparse-from-index (M8/A3b): no directory walk. One stat per
            // indexed path, only read+hash on a stat mismatch. Untracked
            // files are not in `alt diff`'s output (show_added=false), so
            // skipping the dir traversal is correct — same shape git diff
            // uses. Drops `alt diff` on a clean 50k-file monorepo from
            // seconds to ≤100 ms.
            let work = scan_indexed_paths(&self.root, &raw, self.store.algo)?;
            let index = index_entries(&raw);
            (index, work, true, false)
        };

        let changes: Vec<_> = alt_worktree::changes(&old, &new)
            .into_iter()
            // a working-tree file with no index entry is untracked, not a diff
            .filter(|ch| ch.old.is_some() || show_added)
            .collect();

        if json {
            return self.diff_json(&changes, new_on_disk, semantic, out);
        }
        let mut buf = Vec::new();
        for ch in &changes {
            self.emit_file_diff(&mut buf, ch, new_on_disk, semantic)?;
        }
        out.write_all(&buf)?;
        Ok(())
    }

    /// Builds the `diff --json` document: `{schema_version, files:[…]}`, one
    /// entry per changed file with its oids/modes, a `binary` flag, and the
    /// structured hunks (empty for binary files). When `semantic` is set,
    /// each file gains an `ast_diff` object for languages we have a parser
    /// for; the field is `null` otherwise (including non-`--semantic` runs).
    fn diff_json(
        &self,
        changes: &[alt_worktree::Change],
        new_on_disk: bool,
        semantic: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use crate::json::Json;
        let mut files = Vec::with_capacity(changes.len());
        for ch in changes {
            let old_bytes = match ch.old {
                Some(w) => self.blob_bytes(w.oid)?,
                None => Vec::new(),
            };
            let new_bytes = match ch.new {
                Some(w) if new_on_disk => self.read_for(w)?,
                Some(w) => self.blob_bytes(w.oid)?,
                None => Vec::new(),
            };
            let status = match (ch.old, ch.new) {
                (None, _) => "added",
                (_, None) => "deleted",
                _ => "modified",
            };
            let binary = alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes);
            let hunks = if binary {
                Vec::new()
            } else {
                alt_diff::hunks(&old_bytes, &new_bytes, 3)
                    .iter()
                    .map(hunk_json)
                    .collect()
            };
            // A8 B1 (E2): binary files get a structured chunk-diff summary so
            // an agent can answer "how much of this binary is genuinely
            // shared" via the same `--json` surface it uses for text — without
            // the dump-the-bytes blow-up. Text files leave the field null;
            // adding (not replacing) keeps the v1 schema backward-compatible.
            let chunk_diff_field = if binary {
                binary_chunk_diff_json(&old_bytes, &new_bytes)
            } else {
                Json::Null
            };
            // M7-B3: perceptual-style hint for recognised binary kinds
            // (PNG today). Mirrors the human view's "perceptual diff: …"
            // line; `null` for non-image / unknown / text. Additive — v1
            // schema stays backward-compatible.
            let perceptual_diff_field = if binary {
                perceptual_diff_json(&old_bytes, &new_bytes)
            } else {
                Json::Null
            };
            let ast_diff_field = if semantic && !binary {
                ast_diff_json(ch.path.as_bytes(), &old_bytes, &new_bytes)
            } else {
                Json::Null
            };
            let oid = |w: Option<&WorkEntry>| match w {
                Some(w) => Json::str(w.oid.to_string()),
                None => Json::Null,
            };
            let mode = |w: Option<&WorkEntry>| match w {
                Some(w) => Json::str(format!("{:06o}", w.mode)),
                None => Json::Null,
            };
            files.push(Json::Object(vec![
                ("path", Json::str(ch.path)),
                ("status", Json::str(status)),
                ("old_oid", oid(ch.old)),
                ("new_oid", oid(ch.new)),
                ("old_mode", mode(ch.old)),
                ("new_mode", mode(ch.new)),
                ("binary", Json::Bool(binary)),
                ("hunks", Json::Array(hunks)),
                ("chunk_diff", chunk_diff_field),
                ("perceptual_diff", perceptual_diff_field),
                ("ast_diff", ast_diff_field),
            ]));
        }
        let doc = Json::Object(vec![
            ("schema_version", Json::Num(1)),
            ("files", Json::Array(files)),
        ]);
        doc.write(out)?;
        out.write_all(b"\n")?;
        Ok(())
    }

    /// Writes one file's `diff --git` stanza (header + hunks, or a binary
    /// notice) to `buf`. When `semantic` is set and the path has a parser,
    /// the body is replaced by an A8b AST-diff summary instead of the
    /// unified hunks.
    fn emit_file_diff(
        &self,
        buf: &mut Vec<u8>,
        ch: &alt_worktree::Change,
        new_on_disk: bool,
        semantic: bool,
    ) -> Res<()> {
        let path = ch.path;
        let old_bytes = match ch.old {
            Some(w) => self.blob_bytes(w.oid)?,
            None => Vec::new(),
        };
        let new_bytes = match ch.new {
            Some(w) if new_on_disk => self.read_for(w)?,
            Some(w) => self.blob_bytes(w.oid)?,
            None => Vec::new(),
        };

        buf.extend_from_slice(format!("diff --git a/{path} b/{path}\n").as_bytes());
        match (ch.old, ch.new) {
            (None, Some(w)) => {
                buf.extend_from_slice(format!("new file mode {:06o}\n", w.mode).as_bytes());
            }
            (Some(w), None) => {
                buf.extend_from_slice(format!("deleted file mode {:06o}\n", w.mode).as_bytes());
            }
            (Some(o), Some(n)) if o.mode != n.mode => {
                buf.extend_from_slice(format!("old mode {:06o}\n", o.mode).as_bytes());
                buf.extend_from_slice(format!("new mode {:06o}\n", n.mode).as_bytes());
            }
            _ => {}
        }
        let o7 = abbrev(ch.old.map(|w| w.oid));
        let n7 = abbrev(ch.new.map(|w| w.oid));
        buf.extend_from_slice(format!("index {o7}..{n7}\n").as_bytes());

        // A8b: when --semantic is set and we have a parser for this path,
        // render the AST-level summary in place of the unified hunks. A
        // parse error or unsupported language falls through to the regular
        // diff body — the caller never loses signal, only sometimes the
        // semantic resolution.
        if semantic
            && !alt_diff::is_binary(&old_bytes)
            && !alt_diff::is_binary(&new_bytes)
            && let Some(lang) = lang_for(path.as_bytes())
            && let (Ok(old_s), Ok(new_s)) = (
                std::str::from_utf8(&old_bytes),
                std::str::from_utf8(&new_bytes),
            )
            && let Ok(ast) = alt_treediff::tree_diff(old_s, new_s, lang)
        {
            write_ast_diff_summary(buf, &ast);
            return Ok(());
        }

        if alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes) {
            // git-compat line first (existing tests + muscle memory key off
            // this exact wording); then an A8 B1 chunk-diff summary so the
            // human view answers "how much is genuinely shared" too.
            buf.extend_from_slice(
                format!("Binary files a/{path} and b/{path} differ\n").as_bytes(),
            );
            let cd = alt_diff::binary::chunk_diff(
                &old_bytes,
                &new_bytes,
                alt_diff::binary::DEFAULT_PARAMS,
            );
            let pct = (cd.byte_shared_ratio() * 100.0).round() as u32;
            buf.extend_from_slice(
                format!(
                    "chunks: {} shared, {} added, {} removed ({pct}% bytes shared)\n",
                    cd.shared_chunks, cd.added_chunks, cd.removed_chunks,
                )
                .as_bytes(),
            );
            // M7-B3: when both sides are a kind we have a fingerprint for
            // (PNG today), surface the perceptual-style hint so a reader
            // sees "is this a small tweak or a wholly different image"
            // without an external image tool. Stays silent when either
            // side isn't a recognised kind — the chunk-diff line already
            // covers the generic-binary case.
            let old_fp = alt_diff::perceptual::fingerprint(&old_bytes);
            let new_fp = alt_diff::perceptual::fingerprint(&new_bytes);
            if let Some(d) = alt_diff::perceptual::distance(old_fp, new_fp) {
                let kind = old_fp.unwrap().kind.as_str();
                let pct_off = (d * 100.0).round() as u32;
                buf.extend_from_slice(
                    format!("perceptual diff: {pct_off}% off (prism={kind})\n").as_bytes(),
                );
            }
            return Ok(());
        }

        match ch.old {
            Some(_) => buf.extend_from_slice(format!("--- a/{path}\n").as_bytes()),
            None => buf.extend_from_slice(b"--- /dev/null\n"),
        }
        match ch.new {
            Some(_) => buf.extend_from_slice(format!("+++ b/{path}\n").as_bytes()),
            None => buf.extend_from_slice(b"+++ /dev/null\n"),
        }
        alt_diff::write_unified(buf, &old_bytes, &new_bytes, 3);
        Ok(())
    }

    fn blob_bytes(&self, oid: ObjectId) -> Res<Vec<u8>> {
        Ok(self
            .store
            .odb
            .get(&oid)?
            .ok_or("object missing from store")?
            .data)
    }

    /// Look up a ref by full name — pass-through to the shared refs store
    /// (`store_refs_get` is the public, no-mut alias for callers like the
    /// free-standing `clone` helper).
    pub fn store_refs_get(&self, name: &str) -> Option<RefTarget> {
        self.store.refs.get(name).cloned()
    }

    /// Iterate ref `(name, target)` pairs — owned, so the iterator doesn't
    /// borrow the repo for its full lifetime (matters for clone's
    /// drive-multiple-methods flow).
    pub fn store_refs_iter(&self) -> Vec<(String, RefTarget)> {
        self.store
            .refs
            .iter()
            .map(|(n, t)| (n.to_owned(), t.clone()))
            .collect()
    }

    /// Create a local branch at a given oid, one ref tx, no HEAD change —
    /// used by clone to seed `refs/heads/<branch>` from a fetched
    /// remote-tracking tip before switching to it.
    pub fn create_branch_at(&mut self, full_name: &str, oid: ObjectId) -> Res<()> {
        self.commit_refs_unkeyed(
            "branch",
            &[RefChange {
                name: full_name.to_owned(),
                old: None,
                new: Some(RefTarget::Oid(oid)),
            }],
        )?;
        Ok(())
    }

    /// Retarget HEAD to a symbolic ref (e.g. `refs/heads/develop`) — one
    /// ref tx, no working-tree touch. Used by clone when the remote's
    /// default branch isn't the init-default `main`.
    pub fn point_head_at(&mut self, target: &str) -> Res<()> {
        let head_ref = self.head_ref.clone();
        let old = self.store.refs.get(&head_ref).cloned();
        self.commit_refs_unkeyed(
            "switch",
            &[RefChange {
                name: head_ref,
                old,
                new: Some(RefTarget::Symbolic(target.to_owned())),
            }],
        )?;
        Ok(())
    }

    /// Materialise HEAD's tree into the working directory, replacing any
    /// existing tracked files. Used by clone after wiring HEAD to a newly
    /// created branch — `switch`'s "already on this branch" short-circuit
    /// would otherwise skip the checkout. Errors if HEAD doesn't resolve
    /// to a commit (a fresh init with no commits).
    pub fn materialise_head(&mut self) -> Res<()> {
        let target = self.head_entries()?;
        if target.is_empty() {
            return Ok(());
        }
        // start from an empty old work-tree state — clone's target dir is
        // empty by construction, so there's nothing to remove
        self.checkout(&[], &target)?;
        Ok(())
    }

    fn head_branch(&self) -> Res<String> {
        match self.store.refs.get(&self.head_ref) {
            Some(RefTarget::Symbolic(b)) => Ok(b.clone()),
            Some(RefTarget::Oid(_)) => Err("detached HEAD is not supported yet".into()),
            None => Ok("refs/heads/main".to_owned()),
        }
    }

    /// `alt branch`: list branches (no args), create one at HEAD (`name`),
    /// or delete one (`delete`).
    pub fn branch(
        &mut self,
        name: Option<String>,
        delete: Option<String>,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        match (name, delete) {
            (_, Some(target)) => self.delete_branch(&target, out),
            (Some(new), None) => self.create_branch(&new, out),
            (None, None) => self.list_branches(json, out),
        }
    }

    fn list_branches(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let current = self.head_branch()?;
        if json {
            return self.list_branches_json(&current, out);
        }
        for (name, _) in self.store.refs.iter() {
            if let Some(short) = name.strip_prefix("refs/heads/") {
                let mark = if name == current { "* " } else { "  " };
                writeln!(out, "{mark}{short}")?;
            }
        }
        Ok(())
    }

    /// `branch --json`: `{schema_version, current, branches:[{name,current,oid}]}`
    /// — every local branch with its tip oid and which one HEAD is on.
    fn list_branches_json(&self, current: &str, out: &mut impl Write) -> Res<()> {
        use crate::json::Json;
        let mut branches = Vec::new();
        for (name, _) in self.store.refs.iter() {
            if let Some(short) = name.strip_prefix("refs/heads/") {
                let oid = match self.store.refs.resolve(name)? {
                    Some(o) => Json::str(o.to_string()),
                    None => Json::Null,
                };
                branches.push(Json::Object(vec![
                    ("name", Json::str(short)),
                    ("current", Json::Bool(name == current)),
                    ("oid", oid),
                ]));
            }
        }
        let short = current.strip_prefix("refs/heads/").unwrap_or(current);
        let doc = Json::Object(vec![
            ("schema_version", Json::Num(1)),
            ("current", Json::str(short)),
            ("branches", Json::Array(branches)),
        ]);
        doc.write(out)?;
        out.write_all(b"\n")?;
        Ok(())
    }

    fn create_branch(&mut self, name: &str, out: &mut impl Write) -> Res<()> {
        check_branch_name(name)?;
        let full = format!("refs/heads/{name}");
        if self.store.refs.get(&full).is_some() {
            return Err(format!("a branch named '{name}' already exists").into());
        }
        let commit = self
            .store
            .refs
            .resolve(&self.head_branch()?)?
            .ok_or("cannot create a branch before the first commit")?;
        self.commit_refs(
            "branch",
            &[RefChange {
                name: full,
                old: None,
                new: Some(RefTarget::Oid(commit)),
            }],
        )?;
        writeln!(out, "branch '{name}' created at {commit}")?;
        Ok(())
    }

    fn delete_branch(&mut self, name: &str, out: &mut impl Write) -> Res<()> {
        let full = format!("refs/heads/{name}");
        if full == self.head_branch()? {
            return Err(format!("cannot delete branch '{name}': it is the current branch").into());
        }
        let old = self
            .store
            .refs
            .get(&full)
            .cloned()
            .ok_or_else(|| format!("branch '{name}' not found"))?;
        self.commit_refs(
            "branch",
            &[RefChange {
                name: full,
                old: Some(old),
                new: None,
            }],
        )?;
        writeln!(out, "deleted branch '{name}'")?;
        Ok(())
    }

    /// `alt switch <name>`: point HEAD at branch `name` and materialize its
    /// tree into the working tree. `-c` creates the branch first. Switching
    /// to an existing branch requires a clean tree (no staged/unstaged
    /// changes) so no uncommitted work is lost.
    pub fn switch(
        &mut self,
        name: &str,
        create: bool,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let full = format!("refs/heads/{name}");
        let current = self.head_branch()?;

        // emit either the human line or `{schema_version, branch, result}`
        let report = |out: &mut dyn Write, result: &'static str, human: &str| -> Res<()> {
            if json {
                use crate::json::Json;
                crate::json::emit(
                    out,
                    vec![("branch", Json::str(name)), ("result", Json::str(result))],
                )?;
            } else {
                writeln!(out, "{human}")?;
            }
            Ok(())
        };

        if create {
            check_branch_name(name)?;
            if self.store.refs.get(&full).is_some() {
                return Err(format!("a branch named '{name}' already exists").into());
            }
            // a new branch starts at the current commit (if any); the working
            // tree and index carry over unchanged, so no checkout is needed.
            if let Some(commit) = self.store.refs.resolve(&current)? {
                // non-terminal tx of switch-with-create: the move_head below
                // is the one that carries this command's idempotency key.
                self.commit_refs_unkeyed(
                    "branch",
                    &[RefChange {
                        name: full.clone(),
                        old: None,
                        new: Some(RefTarget::Oid(commit)),
                    }],
                )?;
            }
            self.move_head(&current, &full)?;
            return report(
                out,
                "created",
                &format!("Switched to a new branch '{name}'"),
            );
        }

        if self.store.refs.get(&full).is_none() {
            return Err(format!("invalid reference: {name}").into());
        }
        if full == current {
            return report(out, "already_on", &format!("Already on '{name}'"));
        }
        self.ensure_clean("switch")?;

        let target = match self.store.refs.resolve(&full)? {
            Some(commit) => self.commit_entries(commit)?,
            None => Vec::new(), // unborn target: tree becomes empty
        };
        let old = index_entries(&self.index()?);
        self.checkout(&old, &target)?;
        self.move_head(&current, &full)?;
        report(out, "switched", &format!("Switched to branch '{name}'"))
    }

    /// Refuses `action` if any tracked file has staged or unstaged changes, so
    /// a tree-rewriting operation never clobbers uncommitted work.
    fn ensure_clean(&self, action: &str) -> Res<()> {
        let head = self.head_entries()?;
        let raw = self.index()?;
        let index = index_entries(&raw);
        let worktree = scan_worktree_with_index(&self.root, &raw, self.store.algo)?;
        let st = status(&head, &index, &worktree);
        if !st.staged.is_empty() || !st.unstaged.is_empty() {
            return Err(format!(
                "your local changes would be overwritten by {action}; commit them first"
            )
            .into());
        }
        Ok(())
    }

    /// Materializes `target` into the working tree, removing tracked files in
    /// `old` that are absent from `target`, and rewrites the index to match.
    fn checkout(&mut self, old: &[WorkEntry], target: &[WorkEntry]) -> Res<()> {
        use std::collections::HashSet;
        let old_paths: HashSet<&BString> = old.iter().map(|e| &e.path).collect();
        let target_paths: HashSet<&BString> = target.iter().map(|e| &e.path).collect();

        // validate first: an untracked file in the way of a target file is a
        // collision — refuse before touching anything. Gitlinks expect a
        // directory (the submodule placeholder), so an existing dir there is
        // not a collision; let materialize() decide.
        for t in target {
            if t.mode == 0o160000 {
                continue;
            }
            if !old_paths.contains(&t.path) && self.abs(&t.path)?.symlink_metadata().is_ok() {
                return Err(format!(
                    "untracked working tree file '{}' would be overwritten by switch",
                    t.path
                )
                .into());
            }
        }

        for o in old {
            if !target_paths.contains(&o.path) {
                // Gitlinks point at a submodule directory we never managed
                // on-disk; leaving the placeholder alone matches git's
                // `git switch` behaviour (it only removes regular tracked
                // files, not submodule directories).
                if o.mode == 0o160000 {
                    continue;
                }
                let abs = self.abs(&o.path)?;
                if abs.symlink_metadata().is_ok() {
                    std::fs::remove_file(&abs)?;
                    self.prune_empty_dirs(abs.parent());
                }
            }
        }

        let mut entries = Vec::with_capacity(target.len());
        for t in target {
            self.materialize(t)?;
            entries.push(self.make_entry(t)?);
        }
        save_index(
            &self.index_path,
            &Index {
                version: 2,
                entries,
                extensions: Vec::new(),
            },
        )?;
        Ok(())
    }

    /// Writes one tree entry to the working tree (regular file, executable,
    /// or symlink), creating parent dirs and replacing whatever was there.
    /// Gitlink entries (mode 160000) point at a submodule commit that lives
    /// in another repo — there is nothing to write at our odb's level. Match
    /// git's plain `clone` (no `--recurse-submodules`) by ensuring the
    /// placeholder directory exists and stopping there. M7 will grow real
    /// submodule fetch/checkout; A1 only removes the materialise crash.
    fn materialize(&self, w: &WorkEntry) -> Res<()> {
        let abs = self.abs(&w.path)?;
        if w.mode == 0o160000 {
            if abs.symlink_metadata().is_ok_and(|m| !m.is_dir()) {
                std::fs::remove_file(&abs)?;
            }
            std::fs::create_dir_all(&abs)?;
            return Ok(());
        }
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if abs.symlink_metadata().is_ok() {
            std::fs::remove_file(&abs)?;
        }
        let obj = self
            .store
            .odb
            .get(&w.oid)?
            .ok_or("blob missing from store")?;
        if w.mode == 0o120000 {
            symlink_to(&obj.data, &abs)?;
        } else {
            std::fs::write(&abs, &obj.data)?;
            set_exec(&abs, w.mode == 0o100755)?;
        }
        Ok(())
    }

    /// Removes now-empty directories from `dir` up toward the root.
    fn prune_empty_dirs(&self, dir: Option<&Path>) {
        let mut cur = dir;
        while let Some(d) = cur {
            if d == self.root || std::fs::remove_dir(d).is_err() {
                break; // reached the root or a non-empty directory
            }
            cur = d.parent();
        }
    }

    fn abs(&self, rel: &BString) -> Res<PathBuf> {
        Ok(self.root.join(rel.to_path().map_err(|_| "non-utf8 path")?))
    }

    /// Moves HEAD's symbolic target from `from` to `to` in one ref op.
    fn move_head(&mut self, from: &str, to: &str) -> Res<()> {
        let old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        self.commit_refs(
            "switch",
            &[RefChange {
                name: head_ref,
                old: old.or_else(|| Some(RefTarget::Symbolic(from.to_owned()))),
                new: Some(RefTarget::Symbolic(to.to_owned())),
            }],
        )?;
        Ok(())
    }

    /// `alt merge <branch>`: three-way merge `branch` into the current branch.
    /// Returns whether the merge stopped in conflict. Fast-forwards when our
    /// branch is strictly behind; otherwise writes a two-parent merge commit
    /// (clean) or leaves conflict markers + unmerged index entries and makes
    /// no commit (conflicting). A clean working tree is required.
    pub fn merge(&mut self, branch_name: &str, json: bool, out: &mut impl Write) -> Res<bool> {
        let their_ref = format!("refs/heads/{branch_name}");
        let theirs = self
            .store
            .refs
            .resolve(&their_ref)?
            .ok_or_else(|| format!("merge: {branch_name} - not something we can merge"))?;
        let cur = self.head_branch()?;
        let ours = self
            .store
            .refs
            .resolve(&cur)?
            .ok_or("cannot merge into an unborn branch")?;
        self.ensure_clean("merge")?;

        match self.compute_merge(ours, theirs, branch_name)? {
            MergeOutcome::UpToDate => {
                self.report_merge(json, out, "up_to_date", None, &[], "Already up to date.")?;
                Ok(false)
            }
            MergeOutcome::FastForward(commit) => {
                let target = self.commit_entries(commit)?;
                let old = index_entries(&self.index()?);
                self.checkout(&old, &target)?;
                self.advance_branch(&cur, ours, commit)?;
                self.report_merge(
                    json,
                    out,
                    "fast_forward",
                    Some(commit),
                    &[],
                    &format!("Fast-forward to {commit}"),
                )?;
                Ok(false)
            }
            MergeOutcome::Merged { commit, entries } => {
                let old = index_entries(&self.index()?);
                self.checkout(&old, &entries)?;
                self.advance_branch(&cur, ours, commit)?;
                self.report_merge(
                    json,
                    out,
                    "merged",
                    Some(commit),
                    &[],
                    "Merge made by the 'ort' strategy.",
                )?;
                Ok(false)
            }
            MergeOutcome::Conflicted(resolved) => {
                self.write_conflicted(&resolved)?;
                let conflicts: Vec<BString> = resolved
                    .iter()
                    .filter(|r| r.conflicted)
                    .map(|r| r.path.clone())
                    .collect();
                if json {
                    self.report_merge(true, out, "conflicted", None, &conflicts, "")?;
                } else {
                    for p in &conflicts {
                        writeln!(out, "CONFLICT (content): Merge conflict in {p}")?;
                    }
                    writeln!(
                        out,
                        "Automatic merge failed; fix conflicts and then commit the result."
                    )?;
                }
                Ok(true)
            }
        }
    }

    /// Emits one merge outcome: the human line, or `{schema_version, result,
    /// commit?, conflicts}` for `--json`.
    fn report_merge(
        &self,
        json: bool,
        out: &mut impl Write,
        result: &'static str,
        commit: Option<ObjectId>,
        conflicts: &[BString],
        human: &str,
    ) -> Res<()> {
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("result", Json::str(result)),
                    (
                        "commit",
                        match commit {
                            Some(c) => Json::str(c.to_string()),
                            None => Json::Null,
                        },
                    ),
                    (
                        "conflicts",
                        Json::Array(conflicts.iter().map(Json::str).collect()),
                    ),
                ],
            )?;
        } else {
            writeln!(out, "{human}")?;
        }
        Ok(())
    }

    /// Computes how merging `theirs` into `ours` resolves, writing any merged
    /// blobs / tree / merge commit to the object store but touching no refs or
    /// working tree. The caller applies the outcome (advancing refs, checking
    /// out, or recording a conflict) — which lets both `alt merge` and the
    /// atomic `flow finish` share one merge engine.
    fn compute_merge(
        &mut self,
        ours: ObjectId,
        theirs: ObjectId,
        label: &str,
    ) -> Res<MergeOutcome> {
        let base = self.merge_base(ours, theirs)?;
        if base == Some(theirs) {
            return Ok(MergeOutcome::UpToDate);
        }
        if base == Some(ours) {
            return Ok(MergeOutcome::FastForward(theirs));
        }

        let base_entries = match base {
            Some(b) => self.commit_entries(b)?,
            None => Vec::new(), // unrelated histories: empty base
        };
        let ours_entries = self.commit_entries(ours)?;
        let theirs_entries = self.commit_entries(theirs)?;
        let resolved = self.merge_trees(&base_entries, &ours_entries, &theirs_entries, label)?;
        self.store.odb.flush()?;

        if resolved.iter().any(|r| r.conflicted) {
            return Ok(MergeOutcome::Conflicted(resolved));
        }

        let entries: Vec<WorkEntry> = resolved.iter().filter_map(|r| r.entry.clone()).collect();
        let tree = write_tree(&mut self.store.odb, &entries, self.store.algo)?;
        let when = (now_ms() / 1000) as i64;
        let (name, email) = self.id.sig();
        let sig = Sig {
            name,
            email,
            when,
            tz: "+0000",
        };
        let msg = format!("Merge branch '{label}'\n");
        let commit = write_commit(
            &mut self.store.odb,
            tree,
            &[ours, theirs],
            &sig,
            &sig,
            &msg,
            self.store.algo,
        )?;
        self.store.odb.flush()?;
        Ok(MergeOutcome::Merged { commit, entries })
    }

    /// `alt flow init`: create `develop` off `main` (or the current branch's
    /// commit) and switch to it, in one ref transaction.
    pub fn flow_init(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        use crate::json::Json;
        let model = alt_flow::BranchModel::default();
        let dev_ref = format!("refs/heads/{}", model.develop);
        if self.store.refs.get(&dev_ref).is_some() {
            if json {
                crate::json::emit(
                    out,
                    vec![
                        ("develop", Json::str(model.develop)),
                        ("already_initialized", Json::Bool(true)),
                    ],
                )?;
            } else {
                writeln!(out, "flow already initialized (branch '{}')", model.develop)?;
            }
            return Ok(());
        }
        let main_ref = format!("refs/heads/{}", model.main);
        let start = match self.store.refs.resolve(&main_ref)? {
            Some(c) => c,
            None => self
                .store
                .refs
                .resolve(&self.head_branch()?)?
                .ok_or("create an initial commit before 'alt flow init'")?,
        };
        let head_old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        self.commit_refs(
            "flow",
            &[
                RefChange {
                    name: dev_ref.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(start)),
                },
                RefChange {
                    name: head_ref,
                    old: head_old,
                    new: Some(RefTarget::Symbolic(dev_ref.clone())),
                },
            ],
        )?;
        if json {
            crate::json::emit(
                out,
                vec![
                    ("develop", Json::str(model.develop)),
                    ("already_initialized", Json::Bool(false)),
                    ("commit", Json::str(start.to_string())),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Initialized flow: created '{}' at {start}",
                model.develop
            )?;
        }
        Ok(())
    }

    /// `alt flow feature start <name>`: branch `feature/<name>` off `develop`
    /// and switch to it, atomically (one op log entry → O(1) undo).
    pub fn flow_feature_start(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = alt_flow::BranchModel::default().feature(name)?;
        let feat_ref = format!("refs/heads/{}", flow.branch);
        let base_ref = format!("refs/heads/{}", flow.base);
        if self.store.refs.get(&feat_ref).is_some() {
            return Err(format!("a branch named '{}' already exists", flow.branch).into());
        }
        let base = self
            .store
            .refs
            .resolve(&base_ref)?
            .ok_or("develop branch missing; run 'alt flow init' first")?;
        self.ensure_clean("flow start")?;

        // the feature branch starts at develop's commit, so its tree equals
        // develop's; materialize it (a no-op when already on develop)
        let target = self.commit_entries(base)?;
        let old = index_entries(&self.index()?);
        self.checkout(&old, &target)?;

        let head_old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        self.commit_refs(
            "flow",
            &[
                RefChange {
                    name: feat_ref.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(base)),
                },
                RefChange {
                    name: head_ref,
                    old: head_old,
                    new: Some(RefTarget::Symbolic(feat_ref.clone())),
                },
            ],
        )?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("branch", Json::str(&flow.branch)),
                    ("base", Json::str(flow.base)),
                    ("commit", Json::str(base.to_string())),
                ],
            )?;
        } else {
            writeln!(out, "Switched to a new branch '{}'", flow.branch)?;
        }
        Ok(())
    }

    /// `alt flow feature finish <name>`: merge `feature/<name>` into `develop`,
    /// delete the feature branch, and move HEAD to `develop` — all in one
    /// atomic ref transaction (one op log entry → O(1) undo). Aborts if the
    /// merge conflicts.
    pub fn flow_feature_finish(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = alt_flow::BranchModel::default().feature(name)?;
        let feat_ref = format!("refs/heads/{}", flow.branch);
        let dev_ref = format!("refs/heads/{}", flow.target);
        let feat = self
            .store
            .refs
            .resolve(&feat_ref)?
            .ok_or_else(|| format!("no such feature branch '{}'", flow.branch))?;
        let dev = self
            .store
            .refs
            .resolve(&dev_ref)?
            .ok_or("develop branch missing; run 'alt flow init' first")?;
        self.ensure_clean("flow finish")?;

        // merge the feature into develop without yet touching refs/work tree
        let (new_dev, entries) = match self.compute_merge(dev, feat, &flow.branch)? {
            MergeOutcome::UpToDate => (dev, self.commit_entries(dev)?),
            MergeOutcome::FastForward(c) => (c, self.commit_entries(c)?),
            MergeOutcome::Merged { commit, entries } => (commit, entries),
            MergeOutcome::Conflicted(_) => {
                return Err(format!(
                    "merge of '{}' into '{}' has conflicts; \
                     run 'alt merge' on '{}' and resolve manually",
                    flow.branch, flow.target, flow.target
                )
                .into());
            }
        };

        // one atomic op: advance develop, delete the feature, move HEAD
        let head_old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        self.commit_refs(
            "flow",
            &[
                RefChange {
                    name: dev_ref.clone(),
                    old: Some(RefTarget::Oid(dev)),
                    new: Some(RefTarget::Oid(new_dev)),
                },
                RefChange {
                    name: feat_ref.clone(),
                    old: Some(RefTarget::Oid(feat)),
                    new: None,
                },
                RefChange {
                    name: head_ref,
                    old: head_old,
                    new: Some(RefTarget::Symbolic(dev_ref.clone())),
                },
            ],
        )?;

        // bring the working tree to the merged develop
        let old = index_entries(&self.index()?);
        self.checkout(&old, &entries)?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("target", Json::str(&flow.target)),
                    ("commit", Json::str(new_dev.to_string())),
                    ("deleted", Json::str(&flow.branch)),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Merged '{}' into '{}' and deleted it",
                flow.branch, flow.target
            )?;
        }
        Ok(())
    }

    /// `alt flow release start <name>`: branch `release/<name>` off
    /// `develop` and switch to it — same atomic-ref-tx structure as
    /// feature start, just with a different base + branch prefix.
    pub fn flow_release_start(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = alt_flow::BranchModel::default().release(name)?;
        self.flow_topic_start(&flow, json, out)
    }

    /// `alt flow release finish <name>`: merge `release/<name>` into
    /// `main` *and* back-merge `main` into `develop`, delete the release
    /// branch, move HEAD to `develop` — all in one ref-tx + one op-log
    /// entry. A conflict on either merge aborts the whole flow (atomicity
    /// keeps the prior state untouched). M8/C1 reuses the SIGKILL harness.
    pub fn flow_release_finish(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = alt_flow::BranchModel::default().release(name)?;
        let dev_ref = format!("refs/heads/{}", "develop");
        self.flow_topic_finish_dual(&flow, &dev_ref, "release", json, out)
    }

    /// `alt flow hotfix start <name>`: branch `hotfix/<name>` off `main`
    /// and switch to it.
    pub fn flow_hotfix_start(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = alt_flow::BranchModel::default().hotfix(name)?;
        self.flow_topic_start(&flow, json, out)
    }

    /// `alt flow hotfix finish <name>`: merge `hotfix/<name>` into `main`
    /// and back-merge into `develop`, delete the hotfix branch, move HEAD
    /// to `develop`. Same atomic shape as release finish.
    pub fn flow_hotfix_finish(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = alt_flow::BranchModel::default().hotfix(name)?;
        let dev_ref = format!("refs/heads/{}", "develop");
        self.flow_topic_finish_dual(&flow, &dev_ref, "hotfix", json, out)
    }

    /// Shared start path for any flow whose start = "branch <topic> off
    /// <base>, switch HEAD to topic". Single ref-tx, one op-log entry.
    fn flow_topic_start(
        &mut self,
        flow: &alt_flow::Flow,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let topic_ref = format!("refs/heads/{}", flow.branch);
        let base_ref = format!("refs/heads/{}", flow.base);
        if self.store.refs.get(&topic_ref).is_some() {
            return Err(format!("a branch named '{}' already exists", flow.branch).into());
        }
        let base = self.store.refs.resolve(&base_ref)?.ok_or_else(|| {
            format!(
                "base branch '{}' missing; run 'alt flow init' first",
                flow.base
            )
        })?;
        self.ensure_clean("flow start")?;

        let target = self.commit_entries(base)?;
        let old = index_entries(&self.index()?);
        self.checkout(&old, &target)?;

        let head_old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        self.commit_refs(
            "flow",
            &[
                RefChange {
                    name: topic_ref.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(base)),
                },
                RefChange {
                    name: head_ref,
                    old: head_old,
                    new: Some(RefTarget::Symbolic(topic_ref.clone())),
                },
            ],
        )?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("branch", Json::str(&flow.branch)),
                    ("base", Json::str(&flow.base)),
                    ("commit", Json::str(base.to_string())),
                ],
            )?;
        } else {
            writeln!(out, "Switched to a new branch '{}'", flow.branch)?;
        }
        Ok(())
    }

    /// Shared dual-target finish: merge `flow.branch` into `flow.target`
    /// AND back-merge `flow.target` into `back_ref` (develop, by
    /// convention), delete the topic, move HEAD to `back_ref`. All in
    /// one ref-tx. A conflict on either merge aborts.
    fn flow_topic_finish_dual(
        &mut self,
        flow: &alt_flow::Flow,
        back_ref: &str,
        verb: &str,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let topic_ref = format!("refs/heads/{}", flow.branch);
        let target_ref = format!("refs/heads/{}", flow.target);
        let topic_oid = self
            .store
            .refs
            .resolve(&topic_ref)?
            .ok_or_else(|| format!("no such {verb} branch '{}'", flow.branch))?;
        let target_oid = self
            .store
            .refs
            .resolve(&target_ref)?
            .ok_or_else(|| format!("{} branch missing", flow.target))?;
        let back_oid = self
            .store
            .refs
            .resolve(back_ref)?
            .ok_or_else(|| format!("{back_ref} missing; run 'alt flow init' first"))?;
        self.ensure_clean("flow finish")?;

        // Step 1: merge the topic into its target (main, typically). A
        // conflict here aborts without touching any ref.
        let (new_target, target_entries) =
            match self.compute_merge(target_oid, topic_oid, &flow.branch)? {
                MergeOutcome::UpToDate => (target_oid, self.commit_entries(target_oid)?),
                MergeOutcome::FastForward(c) => (c, self.commit_entries(c)?),
                MergeOutcome::Merged { commit, entries } => (commit, entries),
                MergeOutcome::Conflicted(_) => {
                    return Err(format!(
                        "merge of '{}' into '{}' has conflicts; \
                     resolve manually before retrying the {verb} finish",
                        flow.branch, flow.target
                    )
                    .into());
                }
            };

        // Step 2: back-merge the (just-advanced) target into develop, so
        // hotfixes/releases land on both main and develop. Conflict here
        // aborts the whole flow — the prior state is untouched until the
        // single ref-tx below commits.
        let (new_back, back_entries) = if new_target == back_oid {
            // back already points at the same commit (rare but possible
            // when develop == main): no second merge to compute
            (back_oid, target_entries.clone())
        } else {
            match self.compute_merge(back_oid, new_target, &flow.target)? {
                MergeOutcome::UpToDate => (back_oid, self.commit_entries(back_oid)?),
                MergeOutcome::FastForward(c) => (c, self.commit_entries(c)?),
                MergeOutcome::Merged { commit, entries } => (commit, entries),
                MergeOutcome::Conflicted(_) => {
                    return Err(format!(
                        "back-merge of '{}' into '{back_ref}' has conflicts; \
                         resolve manually before retrying the {verb} finish",
                        flow.target
                    )
                    .into());
                }
            }
        };

        // Step 3: one atomic ref-tx — advance target + advance back-ref +
        // delete topic + move HEAD to back-ref. SIGKILL anywhere splits
        // into pre/post only (M8/C0 fixture verifies this for the whole
        // flow family).
        let head_old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        let mut changes = vec![
            RefChange {
                name: target_ref.clone(),
                old: Some(RefTarget::Oid(target_oid)),
                new: Some(RefTarget::Oid(new_target)),
            },
            RefChange {
                name: topic_ref.clone(),
                old: Some(RefTarget::Oid(topic_oid)),
                new: None,
            },
            RefChange {
                name: head_ref,
                old: head_old,
                new: Some(RefTarget::Symbolic(back_ref.to_owned())),
            },
        ];
        // Only include the back-ref change when it actually moved — a
        // RefChange with old == new is a no-op the refs layer would
        // reject as a "no actual change".
        if new_back != back_oid {
            changes.push(RefChange {
                name: back_ref.to_owned(),
                old: Some(RefTarget::Oid(back_oid)),
                new: Some(RefTarget::Oid(new_back)),
            });
        }
        self.commit_refs("flow", &changes)?;

        // bring the working tree onto the back-ref (develop)'s tree
        let old = index_entries(&self.index()?);
        self.checkout(&old, &back_entries)?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("kind", Json::str(verb)),
                    ("target", Json::str(&flow.target)),
                    ("target_commit", Json::str(new_target.to_string())),
                    (
                        "back",
                        Json::str(back_ref.trim_start_matches("refs/heads/")),
                    ),
                    ("back_commit", Json::str(new_back.to_string())),
                    ("deleted", Json::str(&flow.branch)),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Merged '{}' into '{}' and back-merged into '{back_ref}', deleted '{}'",
                flow.branch, flow.target, flow.branch
            )?;
        }
        Ok(())
    }

    /// `alt undo`: invert the most recent ref transaction (restoring the prior
    /// branch/HEAD state) and re-materialize HEAD's tree. The inverse is
    /// itself recorded as an op, so undo is append-only and re-undoable.
    pub fn undo(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        // M8-B1: dispatch on the very last op's payload kind. Ref-tx ops
        // invert their ref changes (the M4 path); index-tx ops restore
        // the prior stage-0 entries for each touched path — A2's "any
        // state-changing op is reversible" extended beyond refs.
        let last = self.store.refs.last_op().ok_or("nothing to undo")?.clone();
        match last.payload.first().copied() {
            Some(alt_refs::PAYLOAD_REF_TX) => self.undo_ref_tx(json, out),
            Some(crate::index_tx::PAYLOAD_INDEX_TX) => self.undo_index_tx(&last.payload, json, out),
            Some(other) => Err(format!("unknown op kind {other:#04x} — nothing to undo").into()),
            None => Err("nothing to undo".into()),
        }
    }

    fn undo_ref_tx(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        let changes = self
            .store
            .refs
            .last_transaction()?
            .ok_or("nothing to undo")?;
        self.ensure_clean("undo")?;

        // capture the current HEAD tree to drive the checkout's removals
        let old = index_entries(&self.index()?);
        let inverse: Vec<RefChange> = changes
            .iter()
            .map(|c| RefChange {
                name: c.name.clone(),
                old: c.new.clone(),
                new: c.old.clone(),
            })
            .collect();
        self.commit_refs("undo", &inverse)?;

        let target = self.head_entries()?;
        self.checkout(&old, &target)?;
        if json {
            use crate::json::Json;
            let refs = changes.iter().map(|c| Json::str(&c.name)).collect();
            crate::json::emit(
                out,
                vec![("undone", Json::Bool(true)), ("refs", Json::Array(refs))],
            )?;
        } else {
            writeln!(out, "Undid the last operation")?;
        }
        Ok(())
    }

    fn undo_index_tx(&mut self, payload: &[u8], json: bool, out: &mut impl Write) -> Res<()> {
        let changes = crate::index_tx::decode(payload).map_err(|e| e.to_string())?;
        // Read current stage-0 entries, then rewrite each touched path
        // back to its `old` side (None = drop the entry, Some = re-create
        // with a zero-stat baseline; the next status walk re-stamps).
        let mut entries: Vec<IndexEntry> = self
            .index()?
            .entries
            .into_iter()
            .filter(|e| e.stage() == 0)
            .collect();
        for ch in &changes {
            entries.retain(|e| e.path != ch.path);
            if let Some((oid, mode)) = ch.old {
                entries.push(IndexEntry {
                    ctime: (0, 0),
                    mtime: (0, 0),
                    dev: 0,
                    ino: 0,
                    mode,
                    uid: 0,
                    gid: 0,
                    size: 0,
                    oid,
                    flags: (ch.path.len().min(0x0FFF)) as u16,
                    extended_flags: None,
                    path: ch.path.clone(),
                });
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        save_index(
            &self.index_path,
            &Index {
                version: 2,
                entries,
                extensions: Vec::new(),
            },
        )?;
        // Record the undo itself so a redo can find it. We use the inverse
        // change list (swap old/new) so chaining undos behaves intuitively
        // (undo of undo = the original state).
        let inverse: Vec<crate::index_tx::IndexChange> = changes
            .iter()
            .map(|c| crate::index_tx::IndexChange {
                path: c.path.clone(),
                old: c.new,
                new: c.old,
            })
            .collect();
        let inv_payload = crate::index_tx::encode(&inverse, self.store.algo);
        let actor = self.id.actor("undo");
        self.store.refs.record_op(&actor, now_ms(), &inv_payload)?;

        if json {
            use crate::json::Json;
            let paths = changes
                .iter()
                .map(|c| Json::str(c.path.to_str_lossy().to_string()))
                .collect();
            crate::json::emit(
                out,
                vec![("undone", Json::Bool(true)), ("paths", Json::Array(paths))],
            )?;
        } else {
            writeln!(out, "Undid the last operation ({} path(s))", changes.len())?;
        }
        Ok(())
    }

    /// `alt remote add <name> <url>`: register a git remote. Writes
    /// `<alt-dir>/remotes/<name>` as a tiny `key=value` text file
    /// (zero-serde, human-editable; same调性 as `.alt/policy`). Duplicate
    /// names are rejected so `alt remote add origin <new-url>` doesn't
    /// silently rewire an existing remote.
    pub fn remote_add(
        &mut self,
        name: &str,
        url: &str,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        check_remote_name(name)?;
        let path = self.remote_path(name);
        if path.exists() {
            return Err(format!("remote '{name}' already exists").into());
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        let fetch = default_fetch_refspec(name);
        let body = format!("url={url}\nfetch={fetch}\n");
        std::fs::write(&path, body)?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("name", Json::str(name)),
                    ("url", Json::str(url)),
                    ("fetch", Json::str(&fetch)),
                ],
            )?;
        } else {
            writeln!(out, "added remote '{name}' → {url}")?;
        }
        Ok(())
    }

    /// `alt remote list`: emit configured remotes (alphabetical). Empty
    /// when no `remotes/` dir exists — a fresh repo has no remotes.
    pub fn remote_list(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let remotes = self.read_remotes()?;
        if json {
            use crate::json::Json;
            let arr = remotes
                .iter()
                .map(|r| {
                    Json::Object(vec![
                        ("name", Json::str(&r.name)),
                        ("url", Json::str(&r.url)),
                        ("fetch", Json::str(&r.fetch)),
                    ])
                })
                .collect();
            crate::json::emit(out, vec![("remotes", Json::Array(arr))])?;
        } else {
            for r in &remotes {
                writeln!(out, "{}\t{}", r.name, r.url)?;
            }
        }
        Ok(())
    }

    /// `alt remote remove <name>`: drop a configured remote. The
    /// `refs/remotes/<name>/*` refs are *not* touched — removing the
    /// remote config doesn't pretend the ingested history never happened.
    pub fn remote_remove(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let path = self.remote_path(name);
        if !path.exists() {
            return Err(format!("no such remote '{name}'").into());
        }
        std::fs::remove_file(&path)?;
        if json {
            use crate::json::Json;
            crate::json::emit(out, vec![("removed", Json::str(name))])?;
        } else {
            writeln!(out, "removed remote '{name}'")?;
        }
        Ok(())
    }

    fn remote_path(&self, name: &str) -> PathBuf {
        self.store.alt_dir.join("remotes").join(name)
    }

    /// `alt identity init [<principal>]`: generate a fresh Ed25519
    /// keypair under `<alt-dir>/identity/<principal>.{pub,sec}`. The
    /// secret file is created with mode 0600; the public file is 0644
    /// (it's safe to share). Refuses to overwrite an existing pair so a
    /// rerun never silently swaps a principal's key.
    pub fn identity_init(
        &self,
        principal: Option<&str>,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let principal = match principal {
            Some(p) => p.to_owned(),
            None => self.id.principal.id.clone(),
        };
        check_principal_id(&principal)?;
        let dir = self.store.alt_dir.join("identity");
        std::fs::create_dir_all(&dir)?;
        let pub_path = dir.join(format!("{principal}.pub"));
        let sec_path = dir.join(format!("{principal}.sec"));
        if pub_path.exists() || sec_path.exists() {
            return Err(
                format!("identity '{principal}' already exists; refuse to overwrite").into(),
            );
        }
        let (sec, pub_) = alt_sign::SecretKey::generate();
        std::fs::write(&pub_path, pub_.to_text())?;
        write_secret_file(&sec_path, sec.to_text().as_bytes())?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("principal", Json::str(&principal)),
                    ("pub_path", Json::str(pub_path.display().to_string())),
                    ("sec_path", Json::str(sec_path.display().to_string())),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Generated Ed25519 identity '{principal}':\n  {}\n  {}",
                pub_path.display(),
                sec_path.display(),
            )?;
        }
        Ok(())
    }

    /// `alt identity list`: emit installed identities (public-side rows
    /// only; secrets are never displayed). Each row shows the principal
    /// id and a short fingerprint (first 16 hex chars of the pubkey).
    pub fn identity_list(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let dir = self.store.alt_dir.join("identity");
        let trust_dir = self.store.alt_dir.join("trust");
        let mut rows = read_pubkey_dir(&dir)?;
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let trust: Vec<(String, alt_sign::PublicKey)> = read_pubkey_dir(&trust_dir)?;
        if json {
            use crate::json::Json;
            let identities: Vec<Json> = rows
                .iter()
                .map(|(name, key)| {
                    let trusted = trust
                        .iter()
                        .any(|(n, k)| n == name && k.as_bytes() == key.as_bytes());
                    Json::Object(vec![
                        ("principal", Json::str(name)),
                        ("fingerprint", Json::str(fingerprint(key))),
                        ("trusted", Json::Bool(trusted)),
                    ])
                })
                .collect();
            crate::json::emit(out, vec![("identities", Json::Array(identities))])?;
        } else {
            for (name, key) in &rows {
                let trusted = trust
                    .iter()
                    .any(|(n, k)| n == name && k.as_bytes() == key.as_bytes());
                let trust_tag = if trusted { " trusted" } else { "" };
                writeln!(out, "{name}\t{}{trust_tag}", fingerprint(key))?;
            }
        }
        Ok(())
    }

    /// `alt identity trust <principal> <pub-file>`: install a public key
    /// into `<alt-dir>/trust/<principal>.pub`. The op verifies the
    /// pub-file parses as an `alt-pubkey-ed25519:` key — a typo in the
    /// path fails loudly here, not at signature-verify time.
    pub fn identity_trust(
        &self,
        principal: &str,
        pub_file: &Path,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        check_principal_id(principal)?;
        let text = std::fs::read_to_string(pub_file)?;
        let key = alt_sign::PublicKey::from_text(&text)
            .map_err(|e| format!("not a valid alt pubkey at '{}': {e}", pub_file.display()))?;
        let dir = self.store.alt_dir.join("trust");
        std::fs::create_dir_all(&dir)?;
        let dst = dir.join(format!("{principal}.pub"));
        std::fs::write(&dst, key.to_text())?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("principal", Json::str(principal)),
                    ("fingerprint", Json::str(fingerprint(&key))),
                    ("trust_path", Json::str(dst.display().to_string())),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Trusted '{principal}' (fingerprint {})",
                fingerprint(&key)
            )?;
        }
        Ok(())
    }

    fn read_remotes(&self) -> Res<Vec<Remote>> {
        let dir = self.store.alt_dir.join("remotes");
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
                .ok_or("non-utf8 remote name")?;
            let body = std::fs::read_to_string(&p)?;
            let mut url = String::new();
            let mut fetch = default_fetch_refspec(&name);
            for line in body.lines() {
                let Some((k, v)) = line.split_once('=') else {
                    continue;
                };
                match k.trim() {
                    "url" => url = v.trim().to_owned(),
                    "fetch" => fetch = v.trim().to_owned(),
                    _ => {} // forward-compat: unknown keys ignored
                }
            }
            out.push(Remote { name, url, fetch });
        }
        Ok(out)
    }

    /// `alt fetch <remote> [refspecs…]`: ls-refs the remote, request the
    /// objects reachable from each matched server ref, ingest the streamed
    /// packfile into the native odb, and update `refs/remotes/<remote>/*`
    /// in one ref transaction (M6/W4).
    ///
    /// The fetch is a self-contained round-trip — `done\n` short-circuits
    /// negotiation, so the server sends acknowledgments-free and we always
    /// receive a full (non-thin) pack. Incremental fetch (real haves) and
    /// thin-pack ingest are follow-up steps once they buy real dogfood.
    ///
    /// Auth: per-remote env vars `ALT_HTTP_USER_<NAME>` +
    /// `ALT_HTTP_TOKEN_<NAME>` (uppercased, hyphen→underscore). Falls back
    /// to anonymous (public read).
    pub fn fetch(
        &mut self,
        remote_name: &str,
        refspecs: &[String],
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let remotes = self.read_remotes()?;
        let remote = remotes
            .iter()
            .find(|r| r.name == remote_name)
            .ok_or_else(|| format!("no such remote '{remote_name}'"))?;

        let specs: Vec<RefSpec> = if refspecs.is_empty() {
            vec![RefSpec::parse(&remote.fetch)?]
        } else {
            refspecs
                .iter()
                .map(|s| RefSpec::parse(s))
                .collect::<Result<_, _>>()?
        };

        let transport = build_transport(&remote.name, &remote.url);

        // capability advertisement — surfaces the protocol version and the
        // server's offered commands; we only need to know v2 is supported
        let ad_bytes = transport.info_refs(alt_wire_http::Service::UploadPack)?;
        let ad =
            alt_wire::parse_capability_advertisement(&mut ad_bytes.as_slice(), "git-upload-pack")?;
        if ad.version != 2 {
            return Err(format!("server speaks protocol v{}, want v2", ad.version).into());
        }
        let object_format = ad.object_format.as_deref();
        let algo = parse_object_format(object_format)?;

        // ls-refs: get the server's full ref list (heads + tags + HEAD)
        let mut ls_body = Vec::new();
        alt_wire::encode_ls_refs_request(
            &mut ls_body,
            &alt_wire::LsRefsRequest {
                symrefs: true,
                peel: true,
                ref_prefixes: vec!["refs/heads/".into(), "refs/tags/".into(), "HEAD".into()],
            },
            object_format,
        )?;
        let ls_resp = transport.command(alt_wire_http::Service::UploadPack, &ls_body)?;
        let server_refs = alt_wire::parse_ls_refs_response(&mut ls_resp.as_slice(), algo)?;

        // refspec match: server refs → local destinations + wants
        let mut updates: Vec<RefUpdate> = Vec::new();
        let mut wants: Vec<ObjectId> = Vec::new();
        // capture the server's HEAD symref target so we can mirror it as
        // `refs/remotes/<name>/HEAD` (what git clones do — a clone needs
        // it to know which branch to check out)
        let server_head_target: Option<String> = server_refs.iter().find_map(|r| {
            if r.name == "HEAD" {
                r.symref_target.clone()
            } else {
                None
            }
        });

        for sref in &server_refs {
            if sref.name == "HEAD" {
                continue; // tracked via symref-target on the matched branch
            }
            for spec in &specs {
                let Some(local_name) = spec.map(&sref.name) else {
                    continue;
                };
                let old = match self.store.refs.get(&local_name) {
                    Some(RefTarget::Oid(o)) => Some(*o),
                    _ => None,
                };
                updates.push(RefUpdate {
                    server_name: sref.name.clone(),
                    local_name,
                    new: sref.oid,
                    old,
                });
                wants.push(sref.oid);
                break;
            }
        }
        // dedup wants (same oid may back several refs)
        wants.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        wants.dedup();

        if wants.is_empty() {
            if json {
                use crate::json::Json;
                crate::json::emit(
                    out,
                    vec![
                        ("remote", Json::str(remote_name)),
                        ("objects", Json::Num(0)),
                        ("refs", Json::Array(Vec::new())),
                    ],
                )?;
            } else {
                writeln!(
                    out,
                    "fetch '{remote_name}': nothing to do (no matching refs)"
                )?;
            }
            return Ok(());
        }

        // haves: tips of every ref we already track for this remote. The
        // server uses these to bound the pack it sends; with `done\n` it
        // skips the negotiation round-trip but still trims unreachable
        // objects from the pack.
        let haves = self.collect_haves(remote_name)?;

        let mut fetch_body = Vec::new();
        alt_wire::encode_fetch_request(
            &mut fetch_body,
            &alt_wire::FetchRequest {
                wants: wants.clone(),
                haves,
                done: true,
                ofs_delta: true,
                no_progress: true,
                ..Default::default()
            },
            object_format,
        )?;
        let fetch_resp = transport.command(alt_wire_http::Service::UploadPack, &fetch_body)?;

        // drain preamble + sideband-wrapped pack stream into a temp .pack
        let pack_dir = self.store.alt_dir.join("pack");
        std::fs::create_dir_all(&pack_dir)?;
        let tmp_pack = pack_dir.join(format!("fetch-{}-incoming.pack", remote_name));
        let mut reader = fetch_resp.as_slice();
        let preamble = alt_wire::read_fetch_preamble(&mut reader, algo)?;
        if preamble.packfile_missing {
            return Err("server returned no packfile (negotiation incomplete)".into());
        }
        let bytes_written = {
            let mut sink = std::fs::File::create(&tmp_pack)?;
            let n = alt_wire::drain_packfile(&mut reader, &mut sink, |_| {})?;
            sink.sync_all()?;
            n
        };
        if bytes_written == 0 {
            // server said "nothing new" — drop the empty file, skip indexing
            let _ = std::fs::remove_file(&tmp_pack);
        } else {
            // index the pack and ingest its objects into the native odb
            let indexed = alt_git_pack::index_pack(&tmp_pack, algo, true)
                .map_err(|e| format!("index-pack failed: {e}"))?;
            let ip = alt_git_pack::IndexedPack::open(&indexed.pack_path, algo)?;
            let idx = ip.idx();
            let mut order: Vec<(u64, u32)> = (0..idx.len())
                .map(|i| (idx.offset_at(i).expect("idx in range"), i))
                .collect();
            order.sort_unstable();
            for (offset, i) in order {
                let obj = ip.read_at(offset)?;
                self.store.odb.put(idx.oid_at(i), obj.kind, &obj.data)?;
            }
            self.store.odb.flush()?;
        }

        // ref transaction: one tx for all the remote-tracking refs (plus
        // the mirrored remote HEAD when the server advertised a symref)
        let mut changes: Vec<RefChange> = updates
            .iter()
            .filter(|u| u.old != Some(u.new))
            .map(|u| RefChange {
                name: u.local_name.clone(),
                old: u.old.map(RefTarget::Oid),
                new: Some(RefTarget::Oid(u.new)),
            })
            .collect();
        if let Some(target) = &server_head_target {
            // map `refs/heads/main` → `refs/remotes/<name>/main`, using the
            // first matching spec; skip when the server's HEAD points at a
            // branch our refspec wouldn't have fetched
            let local_target = specs.iter().find_map(|s| s.map(target));
            if let Some(local_target) = local_target {
                let remote_head_name = format!("refs/remotes/{remote_name}/HEAD");
                let current = match self.store.refs.get(&remote_head_name) {
                    Some(RefTarget::Symbolic(s)) => Some(RefTarget::Symbolic(s.clone())),
                    Some(RefTarget::Oid(o)) => Some(RefTarget::Oid(*o)),
                    None => None,
                };
                let new_target = RefTarget::Symbolic(local_target);
                if current.as_ref() != Some(&new_target) {
                    changes.push(RefChange {
                        name: remote_head_name,
                        old: current,
                        new: Some(new_target),
                    });
                }
            }
        }
        if !changes.is_empty() {
            self.commit_refs("fetch", &changes)?;
        }

        if json {
            use crate::json::Json;
            let refs_json: Vec<Json> = updates
                .iter()
                .map(|u| {
                    Json::Object(vec![
                        ("server", Json::str(&u.server_name)),
                        ("local", Json::str(&u.local_name)),
                        ("oid", Json::str(u.new.to_string())),
                        (
                            "old",
                            match u.old {
                                Some(o) => Json::str(o.to_string()),
                                None => Json::Null,
                            },
                        ),
                    ])
                })
                .collect();
            crate::json::emit(
                out,
                vec![
                    ("remote", Json::str(remote_name)),
                    ("objects", Json::Num(wants.len() as i64)),
                    ("pack_bytes", Json::Num(bytes_written as i64)),
                    ("refs", Json::Array(refs_json)),
                ],
            )?;
        } else {
            writeln!(
                out,
                "fetch '{}': {} ref(s), {} byte(s) of packfile",
                remote_name,
                updates.len(),
                bytes_written
            )?;
            for u in &updates {
                let status = match u.old {
                    None => "new",
                    Some(o) if o == u.new => "up-to-date",
                    Some(_) => "updated",
                };
                writeln!(out, "  {status:>10}  {} -> {}", u.server_name, u.local_name)?;
            }
        }
        Ok(())
    }

    /// `alt push <remote> [<refspec>…]`: send the objects reachable from
    /// the local tips the refspec selects, ask the server to update the
    /// matching refs (M6/W5 — git smart-http v1 receive-pack).
    ///
    /// Refspec forms (no wildcards yet):
    /// - `<src>` → push local `refs/heads/<src>` to remote `refs/heads/<src>`.
    /// - `<src>:<dst>` → push local `<src>` to remote `<dst>`. `<src>` is
    ///   resolved through the same DWIM order as `alt switch`: branch
    ///   short-name, then full ref.
    /// - empty refspec list → push the current branch onto itself.
    ///
    /// Force (`-f`): adds the `+` semantics; the server's own policy
    /// (e.g. branch protection) still decides whether to accept.
    pub fn push(
        &mut self,
        remote_name: &str,
        refspecs: &[String],
        force: bool,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let remotes = self.read_remotes()?;
        let remote = remotes
            .iter()
            .find(|r| r.name == remote_name)
            .ok_or_else(|| format!("no such remote '{remote_name}'"))?;

        let transport = build_transport(&remote.name, &remote.url);

        // v1 ref advertisement: the receive-pack endpoint speaks v1 even
        // when we ask for v2 (push isn't in protocol v2 in upstream git)
        let ad_bytes = transport.info_refs(alt_wire_http::Service::ReceivePack)?;
        let ad = alt_wire::parse_v1_ref_advertisement(&mut ad_bytes.as_slice(), self.store.algo)?;
        if !ad.supports("report-status") {
            return Err("remote does not support report-status".into());
        }

        // resolve the refspecs to (local oid, remote ref name) pairs
        let parsed = if refspecs.is_empty() {
            let branch = self.head_branch()?;
            vec![PushSpec {
                src: branch.clone(),
                dst: branch,
                force,
            }]
        } else {
            refspecs
                .iter()
                .map(|s| PushSpec::parse(s, force))
                .collect::<Result<_, _>>()?
        };

        let mut updates: Vec<alt_wire::RefUpdate> = Vec::new();
        let mut local_tips: Vec<ObjectId> = Vec::new();
        for spec in &parsed {
            let src_full = canonicalise_local_ref(&spec.src);
            let dst_full = canonicalise_remote_ref(&spec.dst);
            let new = match self.store.refs.get(&src_full) {
                Some(RefTarget::Oid(o)) => *o,
                Some(RefTarget::Symbolic(_)) => {
                    return Err(format!("'{src_full}' is symbolic; resolve it first").into());
                }
                None => {
                    return Err(format!("local ref '{src_full}' does not exist").into());
                }
            };
            let old = ad.refs.get(&dst_full).copied();
            updates.push(alt_wire::RefUpdate {
                old,
                new: Some(new),
                name: dst_full,
            });
            local_tips.push(new);
        }

        if updates.iter().all(|u| u.old == u.new) {
            if json {
                use crate::json::Json;
                crate::json::emit(
                    out,
                    vec![
                        ("remote", Json::str(remote_name)),
                        ("updated", Json::Bool(false)),
                        ("refs", Json::Array(Vec::new())),
                    ],
                )?;
            } else {
                writeln!(out, "push '{remote_name}': everything up to date")?;
            }
            return Ok(());
        }

        // M6/W8 — cross-party A6 pre-check: run the local capability
        // policy against the would-be ref changes BEFORE going to the
        // wire. The server enforces its own A6 (which we can't always
        // know up-front), but the local gate stops a forbidden push from
        // leaking the pack body onto the network at all.
        let pseudo_changes: Vec<RefChange> = updates
            .iter()
            .filter(|u| u.old != u.new)
            .map(|u| RefChange {
                name: u.name.clone(),
                old: u.old.map(RefTarget::Oid),
                new: u.new.map(RefTarget::Oid),
            })
            .collect();
        self.ensure_writable("push")?;
        self.ensure_no_force(&pseudo_changes)?;
        self.ensure_push_branch_allowed(&pseudo_changes)?;
        if !force {
            self.ensure_fast_forward(&pseudo_changes)?;
        }

        // compute outgoing object set: reachable(local tips) \ reachable(server tips)
        let repo = alt_repo::Repository::discover(&self.store.alt_dir)?;
        let excludes: Vec<ObjectId> = ad.refs.values().copied().collect();
        let outgoing = repo.reachable_objects(&local_tips, &excludes)?;

        // write a plain pack (no deltas) of the outgoing set into a
        // scratch dir under `<alt-dir>/tmp_push/`; we don't need it as a
        // stored pack, just as the wire body, so it's removed after the
        // request completes
        let pack_dir = self
            .store
            .alt_dir
            .join("tmp_push")
            .join(format!("push-{remote_name}"));
        std::fs::create_dir_all(&pack_dir)?;
        let pack_bytes = if outgoing.is_empty() {
            // a delete-only push or no-op push still needs to send
            // *something* per the v1 protocol — but in practice servers
            // accept an empty body when no commands ship a pack
            Vec::new()
        } else {
            let count =
                u32::try_from(outgoing.len()).map_err(|_| "outgoing object set exceeds u32")?;
            let mut writer = alt_git_pack::PackWriter::create(&pack_dir, self.store.algo, count)?;
            for (oid, kind) in &outgoing {
                let obj = repo
                    .read_object(oid)?
                    .ok_or_else(|| format!("outgoing object {oid} not in odb"))?;
                writer.add(*oid, *kind, &obj.data)?;
            }
            let written = writer.finish()?;
            std::fs::read(&written.pack_path)?
        };
        // best-effort cleanup; not fatal if the dir is busy
        let _ = std::fs::remove_dir_all(&pack_dir);

        // capabilities to declare on the push request
        let agent = format!("agent=alt/{}", env!("CARGO_PKG_VERSION"));
        let mut caps_owned: Vec<String> = vec![
            "report-status".into(),
            "ofs-delta".into(),
            "side-band-64k".into(),
            agent,
        ];
        // W9 — alt-to-alt private extension: when local signing policy
        // is enabled and we have a sec key on disk, sign the canonical
        // push payload and attach `alt-principal=<id>` +
        // `alt-sig=alt-sig-ed25519:<sig>` to the cap list. Git's
        // receive-pack silently ignores unknown caps; an alt server (W10+)
        // looks for the pair and verifies it against its trust store.
        if let Some((principal, sig_text)) = self.maybe_sign_push(&updates)? {
            caps_owned.push(format!("{}={principal}", alt_wire::CAP_ALT_PRINCIPAL));
            caps_owned.push(format!("{}={sig_text}", alt_wire::CAP_ALT_SIG));
        }
        let caps_refs: Vec<&str> = caps_owned.iter().map(String::as_str).collect();

        let mut body = Vec::new();
        alt_wire::encode_push_request(
            &mut body,
            &updates,
            &caps_refs,
            self.store.algo,
            &pack_bytes,
        )?;
        let resp = transport.command(alt_wire_http::Service::ReceivePack, &body)?;

        // we requested side-band-64k, so the report rides on band 1
        let report = alt_wire::parse_report_status_sideband(&mut resp.as_slice(), |_| {})?;

        // surface failures: an unpack error or any ng line fails the
        // command (non-zero exit), so the caller knows nothing on the
        // server moved
        if let Err(reason) = &report.unpack {
            return Err(format!("server rejected push: unpack {reason}").into());
        }
        let mut any_ng = false;
        for s in &report.commands {
            if let alt_wire::CommandStatus::Ng { .. } = s {
                any_ng = true;
            }
        }

        // render and (locally) record the push as an op so it shows up in
        // op-log; the local ref state didn't change, so we don't commit a
        // ref tx — this is purely an audit marker, omitted here to keep
        // scope tight (W5 follow-up: structured "push" op in op-log)
        if json {
            use crate::json::Json;
            let refs_json: Vec<Json> = report
                .commands
                .iter()
                .map(|s| match s {
                    alt_wire::CommandStatus::Ok(name) => {
                        Json::Object(vec![("name", Json::str(name)), ("status", Json::str("ok"))])
                    }
                    alt_wire::CommandStatus::Ng { name, reason } => Json::Object(vec![
                        ("name", Json::str(name)),
                        ("status", Json::str("ng")),
                        ("reason", Json::str(reason)),
                    ]),
                })
                .collect();
            crate::json::emit(
                out,
                vec![
                    ("remote", Json::str(remote_name)),
                    ("objects", Json::Num(outgoing.len() as i64)),
                    ("pack_bytes", Json::Num(pack_bytes.len() as i64)),
                    ("refs", Json::Array(refs_json)),
                ],
            )?;
        } else {
            writeln!(
                out,
                "push '{}': {} object(s), {} byte(s)",
                remote_name,
                outgoing.len(),
                pack_bytes.len()
            )?;
            for s in &report.commands {
                match s {
                    alt_wire::CommandStatus::Ok(name) => writeln!(out, "  ok       {name}")?,
                    alt_wire::CommandStatus::Ng { name, reason } => {
                        writeln!(out, "  rejected {name}: {reason}")?
                    }
                }
            }
        }
        if any_ng {
            return Err("server rejected one or more refs".into());
        }
        Ok(())
    }

    /// Tip oids of every `refs/remotes/<remote>/*` we already track — the
    /// `have` lines for an incremental fetch.
    fn collect_haves(&self, remote_name: &str) -> Res<Vec<ObjectId>> {
        let prefix = format!("refs/remotes/{remote_name}/");
        let mut out = Vec::new();
        for (name, target) in self.store.refs.iter() {
            if !name.starts_with(&prefix) {
                continue;
            }
            if let RefTarget::Oid(o) = target {
                out.push(*o);
            }
        }
        Ok(out)
    }

    /// `alt op-log`: audit-view the op log, newest first. Each entry surfaces
    /// the A5a structured principal (parsed from the actor string) and, for
    /// ref-transaction payloads, the list of ref changes that op made. Other
    /// payload kinds (e.g. import) appear with `ref_changes = null` so the
    /// audit trail is complete even for non-ref ops.
    ///
    /// Reads the on-disk op log directly — no refresh / no lock — so it stays
    /// idempotent and routes through the daemon harmlessly (cf. `log`). A
    /// concurrent writer might add an op between two reads; the audit
    /// snapshot is just truncated, never inconsistent.
    pub fn op_log(
        &self,
        max_count: Option<usize>,
        json: bool,
        verify: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let oplog = alt_oplog::OpLog::open(&self.store.alt_dir.join("oplog"))?;
        let limit = max_count.unwrap_or(usize::MAX);
        // newest first matches `alt log` and what an auditor naturally wants
        let entries: Vec<&alt_oplog::Op> = oplog.ops().iter().rev().take(limit).collect();

        // when --verify is asked, pre-compute the verdict for each op id
        // up front so the per-op rendering doesn't pay an N×M cost
        let verdicts = if verify {
            verify_oplog_signatures(&self.store.alt_dir, &entries)?
        } else {
            std::collections::HashMap::new()
        };

        if json {
            return render_op_log_json(out, &entries, &verdicts);
        }
        for op in &entries {
            let (principal, verb) = Principal::parse_actor(&op.actor);
            let kind_str = match principal.kind {
                PrincipalKind::Human => "human",
                PrincipalKind::Agent => "agent",
            };
            let session = principal.session.as_deref().unwrap_or("-");
            let sig_tag = match verdicts.get(&op.id) {
                Some(SigVerdict::Ok { principal }) => format!(" sig=signed-ok:{principal}"),
                Some(SigVerdict::Unsigned) => " sig=unsigned".to_owned(),
                Some(SigVerdict::Bad { principal }) => format!(" sig=bad-sig:{principal}"),
                Some(SigVerdict::Untrusted { principal }) => {
                    format!(" sig=untrusted:{principal}")
                }
                None => String::new(),
            };
            writeln!(
                out,
                "{ts} {kind_str}:{id} session={sess} verb={verb}{sig_tag}",
                ts = op.timestamp_ms,
                id = principal.id,
                sess = session,
            )?;
            if let Some(tx) = parse_ref_tx_or_none(&op.payload)? {
                for c in &tx.changes {
                    writeln!(
                        out,
                        "  ref {name}: {old} -> {new}",
                        name = c.name,
                        old = render_target(&c.old),
                        new = render_target(&c.new),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Advances `branch` from `old` to `new` in one ref transaction.
    fn advance_branch(&mut self, branch: &str, old: ObjectId, new: ObjectId) -> Res<()> {
        self.commit_refs(
            "merge",
            &[RefChange {
                name: branch.to_owned(),
                old: Some(RefTarget::Oid(old)),
                new: Some(RefTarget::Oid(new)),
            }],
        )?;
        Ok(())
    }

    /// The merge base (lowest common ancestor) of two commits, or `None` for
    /// unrelated histories. Picks one base when several exist (criss-cross
    /// histories aren't a dogfood concern yet).
    fn merge_base(&self, a: ObjectId, b: ObjectId) -> Res<Option<ObjectId>> {
        if a == b {
            return Ok(Some(a));
        }
        let anc_a = self.ancestors(a)?;
        let anc_b = self.ancestors(b)?;
        let common: std::collections::HashSet<ObjectId> =
            anc_a.intersection(&anc_b).copied().collect();
        // a base is a common commit that is not a proper ancestor of another
        // common commit (i.e. a maximal element of the common set)
        let mut bases = common.clone();
        for c in &common {
            for x in self.ancestors(*c)? {
                if x != *c {
                    bases.remove(&x);
                }
            }
        }
        Ok(bases.into_iter().next())
    }

    /// All commits reachable from `start` (inclusive) via parent links.
    fn ancestors(&self, start: ObjectId) -> Res<std::collections::HashSet<ObjectId>> {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![start];
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            let obj = self.store.odb.get(&c)?.ok_or("commit missing from store")?;
            for p in alt_git_codec::Commit::parse(&obj.data)?.parents() {
                if !seen.contains(&p) {
                    stack.push(p);
                }
            }
        }
        Ok(seen)
    }

    /// Three-way merges two trees over their common base, path by path.
    fn merge_trees(
        &mut self,
        base: &[WorkEntry],
        ours: &[WorkEntry],
        theirs: &[WorkEntry],
        their_label: &str,
    ) -> Res<Vec<Resolved>> {
        use std::collections::{BTreeSet, HashMap};
        let map = |es: &[WorkEntry]| -> HashMap<BString, WorkEntry> {
            es.iter().map(|e| (e.path.clone(), e.clone())).collect()
        };
        let (bm, om, tm) = (map(base), map(ours), map(theirs));
        let paths: BTreeSet<BString> = bm
            .keys()
            .chain(om.keys())
            .chain(tm.keys())
            .cloned()
            .collect();

        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let bo = bm.get(&path).cloned();
            let ao = om.get(&path).cloned();
            let to = tm.get(&path).cloned();
            out.push(self.resolve_path(path, bo, ao, to, their_label)?);
        }
        Ok(out)
    }

    /// Resolves one path's three-way state into a clean entry or a conflict.
    fn resolve_path(
        &mut self,
        path: BString,
        bo: Option<WorkEntry>,
        ao: Option<WorkEntry>,
        to: Option<WorkEntry>,
        their_label: &str,
    ) -> Res<Resolved> {
        let same = |x: &Option<WorkEntry>, y: &Option<WorkEntry>| match (x, y) {
            (None, None) => true,
            (Some(p), Some(q)) => p.oid == q.oid && p.mode == q.mode,
            _ => false,
        };
        if same(&ao, &to) {
            return Ok(Resolved::clean(path, ao)); // both agree (incl. both-deleted)
        }
        if same(&ao, &bo) {
            return Ok(Resolved::clean(path, to)); // ours unchanged → take theirs
        }
        if same(&to, &bo) {
            return Ok(Resolved::clean(path, ao)); // theirs unchanged → take ours
        }

        // both sides diverged from base
        match (&ao, &to) {
            (Some(a), Some(t)) => {
                let base_bytes = match &bo {
                    Some(b) => self.blob_bytes(b.oid)?,
                    None => Vec::new(),
                };
                let ours_bytes = self.blob_bytes(a.oid)?;
                let theirs_bytes = self.blob_bytes(t.oid)?;
                let unmergeable = a.mode != t.mode
                    || alt_diff::is_binary(&base_bytes)
                    || alt_diff::is_binary(&ours_bytes)
                    || alt_diff::is_binary(&theirs_bytes);
                if unmergeable {
                    // keep ours in the working tree, record all three stages
                    return Ok(make_conflict(path, bo, ao, to, ours_bytes));
                }
                let labels = alt_merge::Labels {
                    ours: "HEAD",
                    theirs: their_label,
                };
                let m = alt_merge::merge(&base_bytes, &ours_bytes, &theirs_bytes, &labels);
                if m.is_clean() {
                    let oid = ObjectId::hash_object(self.store.algo, ObjectKind::Blob, &m.content);
                    self.store.odb.put(oid, ObjectKind::Blob, &m.content)?;
                    Ok(Resolved::clean(
                        path.clone(),
                        Some(WorkEntry {
                            path,
                            oid,
                            mode: a.mode,
                        }),
                    ))
                } else {
                    Ok(make_conflict(path, bo, ao, to, m.content))
                }
            }
            // modify/delete: one side changed the file, the other removed it
            (Some(a), None) => {
                let bytes = self.blob_bytes(a.oid)?;
                Ok(make_conflict(path, bo, ao, to, bytes))
            }
            (None, Some(t)) => {
                let bytes = self.blob_bytes(t.oid)?;
                Ok(make_conflict(path, bo, ao, to, bytes))
            }
            (None, None) => unreachable!("same(ao, to) already handled both-None"),
        }
    }

    /// Writes a conflicted merge state to disk: each resolution's working-tree
    /// bytes, then an index carrying stage-0 entries for clean paths and
    /// stage 1/2/3 entries for conflicted ones (so git tools see the merge).
    fn write_conflicted(&mut self, resolved: &[Resolved]) -> Res<()> {
        for r in resolved {
            let abs = self.abs(&r.path)?;
            if let Some(bytes) = &r.worktree {
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if abs.symlink_metadata().is_ok() {
                    std::fs::remove_file(&abs)?;
                }
                std::fs::write(&abs, bytes)?;
            } else {
                match &r.entry {
                    Some(e) => self.materialize(e)?,
                    None => {
                        if abs.symlink_metadata().is_ok() {
                            std::fs::remove_file(&abs)?;
                            self.prune_empty_dirs(abs.parent());
                        }
                    }
                }
            }
        }

        let mut entries = Vec::new();
        for r in resolved {
            if r.conflicted {
                for (stage, w) in &r.stages {
                    entries.push(stage_entry(w, *stage));
                }
            } else if let Some(e) = &r.entry {
                entries.push(self.make_entry(e)?);
            }
        }
        save_index(
            &self.index_path,
            &Index {
                version: 2,
                entries,
                extensions: Vec::new(),
            },
        )?;
        Ok(())
    }
}

/// Renders `status` as the stable JSON schema (version 1):
/// `{schema_version, branch, principal:{kind,id,session?}, staged:[…],
/// unstaged:[…], untracked:[…], unmerged:[…], clean}`. `change` ∈
/// `added`/`modified`/`deleted`; `principal` is the A5a actor — added at C4
/// so an agent can self-check "who am I to this repo" via the same
/// machine-first surface it uses for everything else.
fn render_status_json(
    out: &mut impl Write,
    branch: &str,
    principal: &Principal,
    st: &alt_worktree::Status,
    unmerged: &std::collections::BTreeSet<BString>,
) -> Res<()> {
    use crate::json::Json;
    let change = |k: ChangeKind| match k {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
    };
    let entries = |v: &[(BString, ChangeKind)]| {
        Json::Array(
            v.iter()
                .map(|(p, k)| {
                    Json::Object(vec![
                        ("path", Json::str(p)),
                        ("change", Json::str(change(*k))),
                    ])
                })
                .collect(),
        )
    };
    let paths = |it: &mut dyn Iterator<Item = &BString>| Json::Array(it.map(Json::str).collect());
    let clean = unmerged.is_empty()
        && st.staged.is_empty()
        && st.unstaged.is_empty()
        && st.untracked.is_empty();
    let doc = Json::Object(vec![
        ("schema_version", Json::Num(1)),
        ("branch", Json::str(branch)),
        ("principal", principal_json(principal)),
        ("staged", entries(&st.staged)),
        ("unstaged", entries(&st.unstaged)),
        ("untracked", paths(&mut st.untracked.iter())),
        ("unmerged", paths(&mut unmerged.iter())),
        ("clean", Json::Bool(clean)),
    ]);
    doc.write(out)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// JSON shape for an M7-B3 perceptual hint:
/// `{kind:"perceptual_diff", prism, distance}` where `prism` is the
/// content kind (`"png"` today) and `distance` is the [0.0, 1.0]
/// fraction of fingerprint bits that flipped. `Json::Null` when neither
/// side is a recognised image kind. Additive — no v1 schema bump.
fn perceptual_diff_json(old: &[u8], new: &[u8]) -> crate::json::Json {
    use crate::json::Json;
    let old_fp = alt_diff::perceptual::fingerprint(old);
    let new_fp = alt_diff::perceptual::fingerprint(new);
    let Some(d) = alt_diff::perceptual::distance(old_fp, new_fp) else {
        return Json::Null;
    };
    let prism = old_fp.unwrap().kind.as_str();
    // Four decimal places — matches the chunk_diff ratio resolution.
    let distance = (d * 10000.0).round() / 10000.0;
    Json::Object(vec![
        ("kind", Json::str("perceptual_diff")),
        ("prism", Json::str(prism)),
        ("distance", Json::Float(distance)),
    ])
}

/// JSON shape for an A8 B1 binary chunk diff:
/// `{kind:"binary_chunk_diff", shared_chunks, added_chunks, removed_chunks,
/// old_chunks, new_chunks, old_bytes, new_bytes, byte_shared_ratio}`. The
/// `kind` discriminator leaves room for B2 (part-aware) under the same
/// `chunk_diff` field without a v1 schema bump.
fn binary_chunk_diff_json(old: &[u8], new: &[u8]) -> crate::json::Json {
    use crate::json::Json;
    let cd = alt_diff::binary::chunk_diff(old, new, alt_diff::binary::DEFAULT_PARAMS);
    // Round to four decimals — enough resolution for an agent's "is this
    // mostly the same" check without surfacing FP-noise tails in stable
    // schemas (the same `--full` mode that adds chunk OIDs can dial it up).
    let ratio = (cd.byte_shared_ratio() * 10000.0).round() / 10000.0;
    Json::Object(vec![
        ("kind", Json::str("binary_chunk_diff")),
        ("shared_chunks", Json::Num(cd.shared_chunks as i64)),
        ("added_chunks", Json::Num(cd.added_chunks as i64)),
        ("removed_chunks", Json::Num(cd.removed_chunks as i64)),
        ("old_chunks", Json::Num(cd.old_chunks as i64)),
        ("new_chunks", Json::Num(cd.new_chunks as i64)),
        ("old_bytes", Json::Num(cd.old_bytes as i64)),
        ("new_bytes", Json::Num(cd.new_bytes as i64)),
        ("byte_shared_ratio", Json::Float(ratio)),
    ])
}

/// Map a tracked path to the AST-diff language we have a parser for, or
/// `None` for paths we don't (the caller falls back to line/binary diff).
fn lang_for(path: &[u8]) -> Option<alt_treediff::Lang> {
    let p = std::str::from_utf8(path).ok()?;
    if p.ends_with(".rs") {
        Some(alt_treediff::Lang::Rust)
    } else {
        None
    }
}

/// JSON shape for an A8b AST diff:
/// `{kind:"ast_diff", logical_changes:[…], format_only_changes:[…],
/// items_added:[…], items_removed:[…], is_format_only}`. Returns `Null` if
/// the path has no parser, or if parsing failed on either side (the caller
/// then falls through to the line-diff `hunks` field on the same entry,
/// preserving signal — semantic resolution is a refinement, not a contract).
fn ast_diff_json(path: &[u8], old: &[u8], new: &[u8]) -> crate::json::Json {
    use crate::json::Json;
    let Some(lang) = lang_for(path) else {
        return Json::Null;
    };
    let (Ok(old_s), Ok(new_s)) = (std::str::from_utf8(old), std::str::from_utf8(new)) else {
        return Json::Null;
    };
    let Ok(ast) = alt_treediff::tree_diff(old_s, new_s, lang) else {
        return Json::Null;
    };
    let keys = |v: &[alt_treediff::ItemPresence]| {
        Json::Array(v.iter().map(|p| Json::str(&p.key)).collect())
    };
    let changes =
        |v: &[alt_treediff::ItemChange]| Json::Array(v.iter().map(|c| Json::str(&c.key)).collect());
    Json::Object(vec![
        ("kind", Json::str("ast_diff")),
        ("logical_changes", changes(&ast.logical_changes)),
        ("format_only_changes", changes(&ast.format_only_changes)),
        ("items_added", keys(&ast.items_added)),
        ("items_removed", keys(&ast.items_removed)),
        ("is_format_only", Json::Bool(ast.is_format_only())),
    ])
}

/// Human render for an AST diff: one section per kind, key per line.
/// Silent (no output) for an empty diff so an unchanged file produces no
/// stanza body — same shape as the line-diff path.
fn write_ast_diff_summary(buf: &mut Vec<u8>, ast: &alt_treediff::AstDiff) {
    let section = |buf: &mut Vec<u8>, label: &str, items: &[&str]| {
        if items.is_empty() {
            return;
        }
        buf.extend_from_slice(format!("{label}:\n").as_bytes());
        for k in items {
            buf.extend_from_slice(format!("  {k}\n").as_bytes());
        }
    };
    let logical: Vec<&str> = ast.logical_changes.iter().map(|c| c.key.as_str()).collect();
    let fmt_only: Vec<&str> = ast
        .format_only_changes
        .iter()
        .map(|c| c.key.as_str())
        .collect();
    let added: Vec<&str> = ast.items_added.iter().map(|p| p.key.as_str()).collect();
    let removed: Vec<&str> = ast.items_removed.iter().map(|p| p.key.as_str()).collect();
    section(buf, "logical changes", &logical);
    section(buf, "items added", &added);
    section(buf, "items removed", &removed);
    section(buf, "format-only", &fmt_only);
}

/// `alt op-log --json` doc:
/// `{schema_version:1, ops:[{id, timestamp_ms, principal:{…}, verb,
/// ref_changes:[{name, old, new}] | null}]}`. `old`/`new` are `null` for
/// absent, `<oid>` for object targets, `@<name>` for symbolic targets.
fn render_op_log_json(
    out: &mut impl Write,
    ops: &[&alt_oplog::Op],
    verdicts: &std::collections::HashMap<OpId, SigVerdict>,
) -> Res<()> {
    use crate::json::Json;
    let mut entries = Vec::with_capacity(ops.len());
    for op in ops {
        let (principal, verb) = Principal::parse_actor(&op.actor);
        let ref_changes = match parse_ref_tx_or_none(&op.payload)? {
            Some(tx) => Json::Array(
                tx.changes
                    .iter()
                    .map(|c| {
                        Json::Object(vec![
                            ("name", Json::str(&c.name)),
                            ("old", target_json(&c.old)),
                            ("new", target_json(&c.new)),
                        ])
                    })
                    .collect(),
            ),
            None => Json::Null,
        };
        let mut fields = vec![
            ("id", Json::str(hex32(&op.id.0))),
            ("timestamp_ms", Json::Num(op.timestamp_ms as i64)),
            ("principal", principal_json(&principal)),
            ("verb", Json::str(&verb)),
            ("ref_changes", ref_changes),
        ];
        if let Some(v) = verdicts.get(&op.id) {
            let (status, signer): (&'static str, Option<&str>) = match v {
                SigVerdict::Ok { principal } => ("signed-ok", Some(principal.as_str())),
                SigVerdict::Unsigned => ("unsigned", None),
                SigVerdict::Bad { principal } => ("bad-sig", Some(principal.as_str())),
                SigVerdict::Untrusted { principal } => ("untrusted", Some(principal.as_str())),
            };
            fields.push((
                "sig",
                Json::Object(vec![
                    ("status", Json::str(status)),
                    (
                        "principal",
                        match signer {
                            Some(s) => Json::str(s),
                            None => Json::Null,
                        },
                    ),
                ]),
            ));
        }
        entries.push(Json::Object(fields));
    }
    let doc = Json::Object(vec![
        ("schema_version", Json::Num(1)),
        ("ops", Json::Array(entries)),
    ]);
    doc.write(out)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// Try to parse `payload` as a ref-transaction. Wraps `alt_refs::parse_tx`
/// so the caller doesn't import the parser; `None` means "this payload is
/// not a ref tx" (import, future op kinds), not an error.
fn parse_ref_tx_or_none(payload: &[u8]) -> Res<Option<alt_refs::ParsedTx>> {
    Ok(alt_refs::parse_tx(payload)?)
}

fn render_target(t: &Option<RefTarget>) -> String {
    match t {
        None => "null".to_string(),
        Some(RefTarget::Oid(oid)) => oid.to_string(),
        Some(RefTarget::Symbolic(name)) => format!("@{name}"),
    }
}

fn target_json(t: &Option<RefTarget>) -> crate::json::Json {
    use crate::json::Json;
    match t {
        None => Json::Null,
        Some(RefTarget::Oid(oid)) => Json::str(oid.to_string()),
        Some(RefTarget::Symbolic(name)) => Json::str(format!("@{name}")),
    }
}

fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// JSON shape for an A5a principal: `{kind: "human"|"agent", id, session}`
/// (session is `null` when unset). Re-used wherever an actor is surfaced
/// — currently `status --json`; a later op-log viewer will reuse it.
fn principal_json(p: &Principal) -> crate::json::Json {
    use crate::json::Json;
    let kind = match p.kind {
        PrincipalKind::Human => "human",
        PrincipalKind::Agent => "agent",
    };
    Json::Object(vec![
        ("kind", Json::str(kind)),
        ("id", Json::str(&p.id)),
        (
            "session",
            match &p.session {
                Some(s) => Json::str(s),
                None => Json::Null,
            },
        ),
    ])
}

/// One diff hunk as JSON: the `@@` coordinates plus tagged lines, where each
/// line is `{tag: context|add|remove, content}` (content keeps its raw bytes,
/// including any trailing newline).
fn hunk_json(h: &alt_diff::Hunk) -> crate::json::Json {
    use crate::json::Json;
    let lines = h
        .lines
        .iter()
        .map(|(tag, line)| {
            let kind = match tag {
                b'+' => "add",
                b'-' => "remove",
                _ => "context",
            };
            Json::Object(vec![("tag", Json::str(kind)), ("content", Json::str(line))])
        })
        .collect();
    Json::Object(vec![
        ("old_start", Json::Num(h.old_start as i64)),
        ("old_len", Json::Num(h.old_len as i64)),
        ("new_start", Json::Num(h.new_start as i64)),
        ("new_len", Json::Num(h.new_len as i64)),
        ("lines", Json::Array(lines)),
    ])
}

/// How a merge resolves once computed (no refs or work tree touched yet).
enum MergeOutcome {
    /// `theirs` is already an ancestor of `ours`; nothing to do.
    UpToDate,
    /// `ours` is an ancestor of `theirs`; advance to this commit.
    FastForward(ObjectId),
    /// A clean three-way merge: this merge commit and its flattened tree.
    Merged {
        commit: ObjectId,
        entries: Vec<WorkEntry>,
    },
    /// At least one path conflicts; the per-path resolutions to write out.
    Conflicted(Vec<Resolved>),
}

/// One path's merge resolution.
struct Resolved {
    path: BString,
    /// The clean result entry (`None` = deleted); meaningless when conflicted.
    entry: Option<WorkEntry>,
    conflicted: bool,
    /// Bytes to write to the working tree on conflict (markers or the kept
    /// side); `None` for a clean resolution.
    worktree: Option<Vec<u8>>,
    /// Unmerged index entries `(stage, entry)` for a conflict.
    stages: Vec<(u8, WorkEntry)>,
}

impl Resolved {
    fn clean(path: BString, entry: Option<WorkEntry>) -> Self {
        Resolved {
            path,
            entry,
            conflicted: false,
            worktree: None,
            stages: Vec::new(),
        }
    }
}

/// Builds a conflicted resolution: working-tree `bytes` plus stage 1/2/3
/// index entries for whichever of base/ours/theirs are present.
fn make_conflict(
    path: BString,
    bo: Option<WorkEntry>,
    ao: Option<WorkEntry>,
    to: Option<WorkEntry>,
    bytes: Vec<u8>,
) -> Resolved {
    let mut stages = Vec::new();
    if let Some(b) = bo {
        stages.push((1u8, b));
    }
    if let Some(a) = ao {
        stages.push((2u8, a));
    }
    if let Some(t) = to {
        stages.push((3u8, t));
    }
    Resolved {
        path,
        entry: None,
        conflicted: true,
        worktree: Some(bytes),
        stages,
    }
}

/// A zero-stat index entry at a given merge `stage` (1=base, 2=ours,
/// 3=theirs).
fn stage_entry(w: &WorkEntry, stage: u8) -> IndexEntry {
    IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode: w.mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid: w.oid,
        flags: ((stage as u16) << 12) | (w.path.len().min(0x0FFF) as u16),
        extended_flags: None,
        path: w.path.clone(),
    }
}

#[cfg(unix)]
fn symlink_to(target: &[u8], at: &Path) -> Res<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), at)?;
    Ok(())
}

#[cfg(not(unix))]
fn symlink_to(target: &[u8], at: &Path) -> Res<()> {
    // no symlink support: fall back to a regular file holding the target text
    std::fs::write(at, target)?;
    Ok(())
}

#[cfg(unix)]
fn set_exec(path: &Path, exec: bool) -> Res<()> {
    use std::os::unix::fs::PermissionsExt;
    let perm = if exec { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(perm))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_exec(_path: &Path, _exec: bool) -> Res<()> {
    Ok(())
}

/// Minimal git check-ref-format: no empty/space/control chars, no `..`,
/// no leading/trailing slash, no segment starting with a dot.
fn check_branch_name(name: &str) -> Res<()> {
    let bad = name.is_empty()
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains(['\\', ' ', '~', '^', ':', '?', '*', '['])
        || name.bytes().any(|b| b < 0x20 || b == 0x7f)
        || name
            .split('/')
            .any(|seg| seg.is_empty() || seg.starts_with('.'));
    if bad {
        return Err(format!("'{name}' is not a valid branch name").into());
    }
    Ok(())
}

/// Reads a working tree's `.alt` marker file: line 1 is the repo root (the
/// directory holding the real `.alt`), line 2 is the workspace name.
fn parse_workspace_marker(path: &Path) -> Res<(PathBuf, String)> {
    let content = std::fs::read_to_string(path)?;
    let mut lines = content.lines();
    let repo_root = lines.next().ok_or("malformed .alt workspace marker")?;
    let name = lines.next().ok_or("malformed .alt workspace marker")?;
    Ok((PathBuf::from(repo_root), name.to_owned()))
}

/// One configured git remote (parsed from `<alt-dir>/remotes/<name>`).
struct Remote {
    name: String,
    url: String,
    fetch: String,
}

/// One server-ref → local-ref mapping that the fetch will write.
struct RefUpdate {
    /// The ref name on the remote (e.g. `refs/heads/main`).
    server_name: String,
    /// The destination ref locally (e.g. `refs/remotes/origin/main`).
    local_name: String,
    /// New oid from the remote.
    new: ObjectId,
    /// Currently-tracked oid, or `None` if this ref is new locally.
    old: Option<ObjectId>,
}

/// A parsed refspec: `[+]<src>:<dst>` with optional `*` wildcard segments.
/// Only the wildcard form `<prefix>*<suffix>:<prefix>*<suffix>` and exact
/// forms are supported — enough for the canonical default
/// `+refs/heads/*:refs/remotes/<name>/*` plus user-provided exact pairs.
struct RefSpec {
    /// `+` allows non-fast-forward (mirrored from git; the local
    /// `commit_refs` force gate still applies for branches the policy
    /// covers).
    #[allow(dead_code)]
    force: bool,
    src_prefix: String,
    src_suffix: String,
    dst_prefix: String,
    dst_suffix: String,
    /// `true` iff this is a wildcard refspec (`*` appears in both sides).
    wildcard: bool,
}

impl RefSpec {
    fn parse(s: &str) -> Res<Self> {
        let (force, rest) = match s.strip_prefix('+') {
            Some(r) => (true, r),
            None => (false, s),
        };
        let (src, dst) = rest
            .split_once(':')
            .ok_or_else(|| format!("malformed refspec '{s}': missing ':'"))?;
        let src_star = src.find('*');
        let dst_star = dst.find('*');
        match (src_star, dst_star) {
            (Some(si), Some(di)) => Ok(RefSpec {
                force,
                src_prefix: src[..si].to_owned(),
                src_suffix: src[si + 1..].to_owned(),
                dst_prefix: dst[..di].to_owned(),
                dst_suffix: dst[di + 1..].to_owned(),
                wildcard: true,
            }),
            (None, None) => Ok(RefSpec {
                force,
                src_prefix: src.to_owned(),
                src_suffix: String::new(),
                dst_prefix: dst.to_owned(),
                dst_suffix: String::new(),
                wildcard: false,
            }),
            _ => Err(format!("malformed refspec '{s}': wildcard must be on both sides").into()),
        }
    }

    /// Map a server ref name to its local destination, or `None` if the
    /// ref doesn't match this spec.
    fn map(&self, server_ref: &str) -> Option<String> {
        if !self.wildcard {
            if server_ref == self.src_prefix {
                return Some(self.dst_prefix.clone());
            }
            return None;
        }
        let mid = server_ref
            .strip_prefix(&self.src_prefix)?
            .strip_suffix(&self.src_suffix)?;
        Some(format!("{}{}{}", self.dst_prefix, mid, self.dst_suffix))
    }
}

/// A single `<src>[:<dst>]` push spec, optionally prefixed with `+` (or
/// pushed under `-f`) to allow a non-fast-forward server-side update.
/// Wildcards aren't supported yet — explicit refspecs only.
struct PushSpec {
    src: String,
    dst: String,
    #[allow(dead_code)]
    force: bool,
}

impl PushSpec {
    fn parse(s: &str, force_flag: bool) -> Res<Self> {
        let (force, rest) = match s.strip_prefix('+') {
            Some(r) => (true, r),
            None => (false, s),
        };
        let (src, dst) = match rest.split_once(':') {
            Some((s, d)) => (s, d),
            None => (rest, rest),
        };
        if src.is_empty() || dst.is_empty() {
            return Err(format!("malformed push refspec '{s}'").into());
        }
        Ok(PushSpec {
            src: src.to_owned(),
            dst: dst.to_owned(),
            force: force || force_flag,
        })
    }
}

/// Expand a short ref name to its full local form: `main` →
/// `refs/heads/main`. Already-qualified names pass through.
fn canonicalise_local_ref(name: &str) -> String {
    if name.starts_with("refs/") || name == "HEAD" {
        name.to_owned()
    } else {
        format!("refs/heads/{name}")
    }
}

/// Same DWIM but for the destination side: a remote `<branch>` becomes
/// `refs/heads/<branch>` (push never targets `refs/remotes/...`).
fn canonicalise_remote_ref(name: &str) -> String {
    if name.starts_with("refs/") {
        name.to_owned()
    } else {
        format!("refs/heads/{name}")
    }
}

/// Build an [`alt_wire_http::GitTransport`] for `url`, attaching Basic
/// auth from env vars `ALT_HTTP_USER_<NAME>` + `ALT_HTTP_TOKEN_<NAME>` if
/// both are set. Public repos work anonymously; private repos point a
/// user at the env-var convention via a clear error path (auth failures
/// come back as HTTP 401 from the transport).
fn build_transport(remote_name: &str, url: &str) -> alt_wire_http::GitTransport {
    let env_key = remote_name
        .chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect::<String>();
    let user = std::env::var(format!("ALT_HTTP_USER_{env_key}")).ok();
    let token = std::env::var(format!("ALT_HTTP_TOKEN_{env_key}")).ok();
    let mut t = alt_wire_http::GitTransport::new(url);
    if let (Some(user), Some(token)) = (user, token) {
        t = t.with_auth(alt_wire_http::BasicAuth {
            username: user,
            token,
        });
    }
    t
}

/// Map the server-advertised `object-format=<algo>` to a [`HashAlgo`].
/// Default (server didn't advertise) is sha-1, matching git.
fn parse_object_format(advertised: Option<&str>) -> Res<HashAlgo> {
    match advertised {
        None | Some("sha1") => Ok(HashAlgo::Sha1),
        Some("sha256") => Ok(HashAlgo::Sha256),
        Some(other) => Err(format!("unsupported object-format '{other}'").into()),
    }
}

/// A5b op-level signing policy, read from `<alt-dir>/sign-policy`. The
/// file is a tiny `key=value` text shape (same convention as remotes /
/// policy elsewhere in alt):
///
/// ```text
/// enabled = true
/// principal = alice           # optional; defaults to the caller's id
/// ```
///
/// Missing file = signing disabled. `enabled` is the only required key;
/// `principal` lets a repo pin signing to a fixed identity even when
/// multiple agents share the working tree.
struct SignPolicy {
    enabled: bool,
    principal: Option<String>,
}

impl SignPolicy {
    fn load(alt_dir: &Path) -> Res<Self> {
        let path = alt_dir.join("sign-policy");
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SignPolicy {
                    enabled: false,
                    principal: None,
                });
            }
            Err(e) => return Err(e.into()),
        };
        let mut enabled = false;
        let mut principal = None;
        for line in body.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k.trim() {
                "enabled" => enabled = v.trim() == "true",
                "principal" => principal = Some(v.trim().to_owned()),
                _ => {} // forward-compat
            }
        }
        Ok(SignPolicy { enabled, principal })
    }
}

/// One parsed signature row from `<alt-dir>/oplog/sigs.log`. Each row is
/// `<op-id-hex> <principal> alt-sig-ed25519:<base64url>\n`.
#[derive(Debug, Clone)]
struct SigRow {
    op_id: OpId,
    principal: String,
    sig: alt_sign::Sig,
}

fn read_sig_rows(alt_dir: &Path) -> Res<Vec<SigRow>> {
    let path = alt_dir.join("oplog").join("sigs.log");
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut rows = Vec::new();
    for line in body.lines() {
        let mut parts = line.splitn(3, ' ');
        let Some(op_hex) = parts.next() else { continue };
        let Some(principal) = parts.next() else {
            continue;
        };
        let Some(sig_text) = parts.next() else {
            continue;
        };
        let mut op_bytes = [0u8; 32];
        if op_hex.len() != 64 {
            continue;
        }
        let mut ok = true;
        for (i, b) in op_bytes.iter_mut().enumerate() {
            let hi = nibble(op_hex.as_bytes()[i * 2]);
            let lo = nibble(op_hex.as_bytes()[i * 2 + 1]);
            match (hi, lo) {
                (Some(h), Some(l)) => *b = (h << 4) | l,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        let Ok(sig) = alt_sign::Sig::from_text(sig_text) else {
            continue;
        };
        rows.push(SigRow {
            op_id: OpId(op_bytes),
            principal: principal.to_owned(),
            sig,
        });
    }
    Ok(rows)
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// The four possible outcomes of `alt op-log --verify` for one op.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SigVerdict {
    /// Signature matched a trusted principal — auditor can rely on this op.
    Ok { principal: String },
    /// No signature row exists for this op (signing was off when the op
    /// was written, or a different principal performed it).
    Unsigned,
    /// Signature row exists but Ed25519 verification failed against the
    /// claimed principal's pubkey — tampering, key swap, or corruption.
    Bad { principal: String },
    /// Signature row exists and parses, but no matching pubkey lives in
    /// `<alt-dir>/trust/<principal>.pub` — auditor can't decide.
    Untrusted { principal: String },
}

/// Walk the sig sidecar + trust store; for each requested op id, decide
/// which [`SigVerdict`] applies. Stops short of fetching pubkeys lazily —
/// for an op-log of a few hundred entries, loading all trust pubkeys
/// up-front is cheaper than `O(N×M)` re-reads.
fn verify_oplog_signatures(
    alt_dir: &Path,
    entries: &[&alt_oplog::Op],
) -> Res<std::collections::HashMap<OpId, SigVerdict>> {
    let rows = read_sig_rows(alt_dir)?;
    let trust = read_pubkey_dir(&alt_dir.join("trust"))?;
    let trust_map: std::collections::BTreeMap<String, alt_sign::PublicKey> =
        trust.into_iter().collect();
    let by_id: std::collections::HashMap<OpId, &SigRow> =
        rows.iter().map(|r| (r.op_id, r)).collect();

    let mut out = std::collections::HashMap::new();
    for op in entries {
        let verdict = match by_id.get(&op.id) {
            None => SigVerdict::Unsigned,
            Some(row) => match trust_map.get(&row.principal) {
                None => SigVerdict::Untrusted {
                    principal: row.principal.clone(),
                },
                Some(pubkey) => match pubkey.verify(&op.id.0, &row.sig) {
                    Ok(()) => SigVerdict::Ok {
                        principal: row.principal.clone(),
                    },
                    Err(_) => SigVerdict::Bad {
                        principal: row.principal.clone(),
                    },
                },
            },
        };
        out.insert(op.id, verdict);
    }
    Ok(out)
}

/// A principal id is the same shape as a remote name: a single path
/// segment with no separators or special chars (it becomes part of a
/// file name under `<alt-dir>/identity/<id>.pub`).
fn check_principal_id(name: &str) -> Res<()> {
    let bad = name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains(['\\', ' ', '~', '^', ':', '?', '*', '[', '.'])
        || name.bytes().any(|b| b < 0x20 || b == 0x7f);
    if bad {
        return Err(format!("'{name}' is not a valid principal id").into());
    }
    Ok(())
}

/// Write a secret-bearing file with restrictive permissions (0600 on
/// Unix; default on other platforms — alt's primary targets are
/// macOS/Linux). Uses an atomic temp+rename so a crashed write doesn't
/// leave a half-written secret in place.
fn write_secret_file(path: &Path, body: &[u8]) -> Res<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(body)?;
        f.sync_all()?;
    }
    set_secret_perms(&tmp)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn set_secret_perms(path: &Path) -> Res<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_secret_perms(_path: &Path) -> Res<()> {
    Ok(())
}

/// Scan a directory for `*.pub` files, parse each as an
/// `alt-pubkey-ed25519:` key. Files that don't parse are silently
/// skipped — they may be stray files; loud parsing happens at `trust`
/// time, not at `list` time.
fn read_pubkey_dir(dir: &Path) -> Res<Vec<(String, alt_sign::PublicKey)>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("pub") {
            continue;
        }
        let name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(key) = alt_sign::PublicKey::from_text(&text) {
            out.push((name, key));
        }
    }
    Ok(out)
}

/// First 16 hex chars of the raw 32-byte public key — short enough to
/// fit a list row, unique enough in practice for human identification.
fn fingerprint(key: &alt_sign::PublicKey) -> String {
    let bytes = key.as_bytes();
    let mut out = String::with_capacity(16);
    for b in &bytes[..8] {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// A remote name: a single path segment (it becomes a file name). Same
/// shape rules as a workspace name, minus the workspace-specific reserved
/// default — so `origin`, `upstream`, `my-fork` are fine.
fn check_remote_name(name: &str) -> Res<()> {
    let bad = name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains(['\\', ' ', '~', '^', ':', '?', '*', '[', '.'])
        || name.bytes().any(|b| b < 0x20 || b == 0x7f);
    if bad {
        return Err(format!("'{name}' is not a valid remote name").into());
    }
    Ok(())
}

/// The default fetch refspec for a new remote: every branch on the
/// remote lands under `refs/remotes/<name>/*` locally. The `+` marker
/// allows non-fast-forward updates of the remote-tracking ref (matching
/// git's default behavior).
fn default_fetch_refspec(name: &str) -> String {
    format!("+refs/heads/*:refs/remotes/{name}/*")
}

/// A workspace name: a single path segment (it becomes part of the ref name
/// `workspaces/<name>/HEAD` and a directory), so no slashes, dots, control or
/// special chars, and not the reserved default name.
fn check_workspace_name(name: &str) -> Res<()> {
    let bad = name.is_empty()
        || name == DEFAULT_WORKSPACE
        || name.starts_with('.')
        || name.contains('/')
        || name.contains(['\\', ' ', '~', '^', ':', '?', '*', '[', '.'])
        || name.bytes().any(|b| b < 0x20 || b == 0x7f);
    if bad {
        return Err(format!("'{name}' is not a valid workspace name").into());
    }
    Ok(())
}

/// A 7-hex-char object id abbreviation, or all-zero for an absent side.
fn abbrev(oid: Option<ObjectId>) -> String {
    match oid {
        Some(o) => o.to_string()[..7].to_owned(),
        None => "0000000".to_owned(),
    }
}

fn empty_index() -> Index {
    Index {
        version: 2,
        entries: Vec::new(),
        extensions: Vec::new(),
    }
}

/// Atomic index write to `path`: temp file + rename (sibling temp).
fn save_index(path: &Path, index: &Index) -> Res<()> {
    let bytes = index.serialize(HashAlgo::Sha1);
    let tmp = path.with_extension("tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Builds an index entry for a working-tree file, filling stat from its
/// metadata so git reads our index without spurious "modified".
#[cfg(unix)]
fn stat_entry(meta: &std::fs::Metadata, w: &WorkEntry) -> IndexEntry {
    use std::os::unix::fs::MetadataExt;
    IndexEntry {
        ctime: (meta.ctime() as u32, meta.ctime_nsec() as u32),
        mtime: (meta.mtime() as u32, meta.mtime_nsec() as u32),
        dev: meta.dev() as u32,
        ino: meta.ino() as u32,
        mode: w.mode,
        uid: meta.uid(),
        gid: meta.gid(),
        size: meta.size() as u32,
        oid: w.oid,
        flags: (w.path.len().min(0x0FFF)) as u16,
        extended_flags: None,
        path: w.path.clone(),
    }
}

#[cfg(not(unix))]
fn stat_entry(_meta: &std::fs::Metadata, w: &WorkEntry) -> IndexEntry {
    IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode: w.mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid: w.oid,
        flags: (w.path.len().min(0x0FFF)) as u16,
        extended_flags: None,
        path: w.path.clone(),
    }
}

/// Zero-stat index entry for a gitlink (mode 160000). Git emits the same
/// shape: there is no on-disk file to stat, only the recorded submodule
/// commit oid. Keeps the index byte-comparable across alt and git.
fn gitlink_entry(w: &WorkEntry) -> IndexEntry {
    IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode: w.mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid: w.oid,
        flags: (w.path.len().min(0x0FFF)) as u16,
        extended_flags: None,
        path: w.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens an owning handle on `root`'s default (or named) workspace.
    fn open(root: &Path, ws: Option<&str>) -> OpenRepo {
        OpenRepo::discover(root, ws, Identity::from_env()).unwrap()
    }

    /// The canonical wildcard refspec maps `refs/heads/<X>` →
    /// `refs/remotes/<remote>/<X>` for any branch name `<X>`.
    #[test]
    fn refspec_wildcard_maps_heads_to_remote_tracking() {
        let spec = RefSpec::parse("+refs/heads/*:refs/remotes/origin/*").unwrap();
        assert!(spec.wildcard);
        assert_eq!(
            spec.map("refs/heads/main").as_deref(),
            Some("refs/remotes/origin/main")
        );
        assert_eq!(
            spec.map("refs/heads/feature/x").as_deref(),
            Some("refs/remotes/origin/feature/x")
        );
        // out of scope: tags don't match a heads-only spec
        assert_eq!(spec.map("refs/tags/v1"), None);
        // out of scope: a ref that doesn't share the prefix
        assert_eq!(spec.map("HEAD"), None);
    }

    /// An exact (non-wildcard) refspec maps exactly one ref name; anything
    /// else returns `None`.
    #[test]
    fn refspec_exact_match_is_one_to_one() {
        let spec = RefSpec::parse("refs/heads/main:refs/remotes/origin/main").unwrap();
        assert!(!spec.wildcard);
        assert_eq!(
            spec.map("refs/heads/main").as_deref(),
            Some("refs/remotes/origin/main")
        );
        assert_eq!(spec.map("refs/heads/dev"), None);
    }

    /// A malformed refspec (missing `:` or one-sided wildcard) is a typed
    /// error, not a silent miss.
    #[test]
    fn refspec_malformed_input_is_rejected() {
        assert!(RefSpec::parse("refs/heads/main").is_err());
        assert!(RefSpec::parse("refs/heads/*:refs/remotes/origin/main").is_err());
        assert!(RefSpec::parse("refs/heads/main:refs/remotes/origin/*").is_err());
    }

    /// `alt clone` derives the target directory from the URL's last path
    /// segment, mirroring git's behaviour: strip `.git`, strip trailing
    /// slashes. Empty / unusable URLs → `None` (caller errors out).
    #[test]
    fn derive_clone_dir_strips_git_suffix_and_slashes() {
        assert_eq!(
            derive_clone_dir("https://github.com/user/repo.git").as_deref(),
            Some("repo")
        );
        assert_eq!(
            derive_clone_dir("https://github.com/user/repo.git/").as_deref(),
            Some("repo")
        );
        assert_eq!(
            derive_clone_dir("https://example.com/myproj").as_deref(),
            Some("myproj")
        );
        // ssh-like form: the part after the last `/`
        assert_eq!(
            derive_clone_dir("git@github.com:user/repo.git").as_deref(),
            Some("repo")
        );
        // empty URL has no segment to derive from
        assert_eq!(derive_clone_dir(""), None);
    }

    /// A named workspace gets its own HEAD, index, and working tree, while the
    /// odb and branch refs stay shared — so work in one does not disturb the
    /// other.
    #[test]
    fn workspaces_isolate_head_index_and_working_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut sink = Vec::new();

        // default workspace: first commit on main, then a `feat` branch
        init(Some(root.to_path_buf()), &mut sink).unwrap();
        std::fs::write(root.join("a.txt"), "main\n").unwrap();
        let mut o = open(root, None);
        let mut repo = o.repo();
        repo.add(&[".".to_owned()], false, &mut sink).unwrap();
        repo.commit("first", false, &mut sink).unwrap();
        repo.branch(Some("feat".to_owned()), None, false, &mut sink)
            .unwrap();

        // a second workspace on `feat`, in its own working tree outside the repo
        let wt_tmp = tempfile::tempdir().unwrap();
        let wt = wt_tmp.path().join("ws2-tree");
        repo.create_workspace("ws2", &wt, "feat").unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).unwrap(),
            "main\n",
            "new workspace materializes its branch tree"
        );

        // commit in ws2 (advancing feat) — the default workspace is untouched
        std::fs::write(wt.join("a.txt"), "feat-work\n").unwrap();
        let mut o2 = open(root, Some("ws2"));
        let mut ws2 = o2.repo();
        ws2.add(&[".".to_owned()], false, &mut sink).unwrap();
        ws2.commit("ws2 work", false, &mut sink).unwrap();

        // default workspace: still on main, working tree and HEAD unchanged
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "main\n"
        );
        let mut od = open(root, None);
        let def = od.repo();
        assert_eq!(def.head_branch().unwrap(), "refs/heads/main");
        assert_eq!(
            open(root, Some("ws2")).repo().head_branch().unwrap(),
            "refs/heads/feat",
            "ws2 is on feat"
        );

        // shared store: the default workspace sees feat advanced by ws2
        assert!(def.store.refs.resolve("refs/heads/feat").unwrap().is_some());

        // listing shows both; removing ws2 drops it
        let names: Vec<String> = def
            .list_workspaces()
            .unwrap()
            .into_iter()
            .map(|(n, ..)| n)
            .collect();
        assert!(names.contains(&"default".to_owned()));
        assert!(names.contains(&"ws2".to_owned()));
        open(root, None).repo().remove_workspace("ws2").unwrap();
        assert!(OpenRepo::discover(root, Some("ws2"), Identity::from_env()).is_err());
    }

    #[test]
    fn the_default_workspace_cannot_be_removed_and_bad_names_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut sink = Vec::new();
        init(Some(root.to_path_buf()), &mut sink).unwrap();
        std::fs::write(root.join("a.txt"), "x\n").unwrap();
        let mut o = open(root, None);
        let mut repo = o.repo();
        repo.add(&[".".to_owned()], false, &mut sink).unwrap();
        repo.commit("c", false, &mut sink).unwrap();

        assert!(repo.remove_workspace("default").is_err());
        let wt = root.join("bad");
        assert!(repo.create_workspace("with/slash", &wt, "main").is_err());
        assert!(repo.create_workspace("default", &wt, "main").is_err());
        assert!(repo.create_workspace("ok", &wt, "no-such-branch").is_err());
    }

    /// `actor_string` and `parse_actor` are mutual inverses for the structured
    /// A5a form — same principal+verb survive an encode/decode round-trip,
    /// including the optional `session` correlator.
    #[test]
    fn principal_actor_roundtrip() {
        for case in [
            Principal {
                kind: PrincipalKind::Agent,
                id: "claude-opus-4-8".into(),
                session: Some("01J7XYZ".into()),
            },
            Principal {
                kind: PrincipalKind::Human,
                id: "alice".into(),
                session: None,
            },
        ] {
            for verb in ["commit", "flow", "switch"] {
                let s = case.actor_string("doracawl", verb);
                let (decoded, dverb) = Principal::parse_actor(&s);
                assert_eq!(decoded, case, "round-trip principal mismatch ({s})");
                assert_eq!(dverb, verb, "round-trip verb mismatch ({s})");
            }
        }
    }

    /// Op-log entries written by alt before A5a use `cli/<verb>@<user>` (the
    /// pre-structured form). They must still parse as a Human principal with
    /// `id == user`, so importing/reading older repositories is lossless.
    #[test]
    fn principal_actor_legacy_compat() {
        let (p, verb) = Principal::parse_actor("cli/commit@alice");
        assert_eq!(p.kind, PrincipalKind::Human);
        assert_eq!(p.id, "alice");
        assert_eq!(p.session, None);
        assert_eq!(verb, "commit");

        let (p, verb) = Principal::parse_actor("cli/flow@bob");
        assert_eq!(p.kind, PrincipalKind::Human);
        assert_eq!(p.id, "bob");
        assert_eq!(verb, "flow");
    }

    /// `Identity::from_lookup` reads the new A5a env vars when present and
    /// degrades to a Human principal anchored on `USER` when they are not —
    /// so pre-A5a callers see exactly the old behaviour (defaults preserved).
    #[test]
    fn identity_reads_alt_principal_env_with_defaults() {
        let id = Identity::from_lookup(|k| match k {
            "USER" => Some("alice".into()),
            _ => None,
        });
        assert_eq!(id.principal.kind, PrincipalKind::Human);
        assert_eq!(id.principal.id, "alice");
        assert_eq!(id.principal.session, None);
        assert_eq!(id.actor("commit"), "human:alice;user:alice;verb:commit");

        let id = Identity::from_lookup(|k| match k {
            "USER" => Some("alice".into()),
            "ALT_PRINCIPAL_KIND" => Some("agent".into()),
            "ALT_PRINCIPAL_ID" => Some("claude-opus-4-8".into()),
            "ALT_SESSION_ID" => Some("01J7".into()),
            _ => None,
        });
        assert_eq!(id.principal.kind, PrincipalKind::Agent);
        assert_eq!(id.principal.id, "claude-opus-4-8");
        assert_eq!(id.principal.session.as_deref(), Some("01J7"));
        let (p, verb) = Principal::parse_actor(&id.actor("commit"));
        assert_eq!(p, id.principal, "actor round-trips back to principal");
        assert_eq!(verb, "commit");

        // Unknown kind value falls back to Human (defensive: env mis-set
        // should not invent an Agent identity the caller did not declare).
        let id = Identity::from_lookup(|k| match k {
            "USER" => Some("alice".into()),
            "ALT_PRINCIPAL_KIND" => Some("robot".into()),
            _ => None,
        });
        assert_eq!(id.principal.kind, PrincipalKind::Human);
    }
}
