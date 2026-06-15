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
    ChangeKind, Sig, WorkEntry, flatten_tree, index_entries, scan_worktree, status, write_commit,
    write_tree,
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
        Ok(id)
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
        let scan = scan_worktree(&self.root, self.store.algo)?;
        let staging_all = paths.iter().any(|p| p == ".");

        // start from the current stage-0 entries (or empty when restaging all)
        let mut entries: Vec<IndexEntry> = if staging_all {
            Vec::new()
        } else {
            self.index()?
                .entries
                .into_iter()
                .filter(|e| e.stage() == 0)
                .collect()
        };

        let mut staged = 0;
        let targets: Vec<BString> = if staging_all {
            scan.iter().map(|w| w.path.clone()).collect()
        } else {
            paths.iter().map(|p| BString::from(p.as_str())).collect()
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
                entries,
                extensions: Vec::new(),
            },
        )?;
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
        let commit = write_commit(
            &mut self.store.odb,
            tree,
            &parents,
            &sig,
            &sig,
            &msg,
            self.store.algo,
        )?;
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
        let worktree = scan_worktree(&self.root, self.store.algo)?;
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
            let index = index_entries(&self.index()?);
            let work = scan_worktree(&self.root, self.store.algo)?;
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
        let index = index_entries(&self.index()?);
        let worktree = scan_worktree(&self.root, self.store.algo)?;
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
        // collision — refuse before touching anything.
        for t in target {
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
    fn materialize(&self, w: &WorkEntry) -> Res<()> {
        let abs = self.abs(&w.path)?;
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

    /// `alt undo`: invert the most recent ref transaction (restoring the prior
    /// branch/HEAD state) and re-materialize HEAD's tree. The inverse is
    /// itself recorded as an op, so undo is append-only and re-undoable.
    pub fn undo(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens an owning handle on `root`'s default (or named) workspace.
    fn open(root: &Path, ws: Option<&str>) -> OpenRepo {
        OpenRepo::discover(root, ws, Identity::from_env()).unwrap()
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
