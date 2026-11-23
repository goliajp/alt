//! Revisions as git spells them.
//!
//! A revision is a full or shortened id or a ref name found through
//! git's lookup order (`@` alone is `HEAD`), then any of:
//!
//! - `<rev>~N`: N steps along first parents, one where N is left out
//! - `<rev>^N`: the N-th parent, the first where N is left out; `^0` is
//!   the commit itself, after peeling any tag
//! - `<rev>^{commit}`, `^{tree}`, `^{}`: the object peeled to that kind,
//!   `^{}` to whatever a tag names
//! - `<rev>@{u}`, `<rev>@{upstream}`: the remote-tracking branch the
//!   branch follows, the current branch where none is named
//! - `<ref>@{N}`: what the ref named N moves ago, as its log tells;
//!   `@{N}` alone is of the current branch
//! - `@{-N}`: the branch that was left the N-th last time one was
//!   switched from
//!
//! and last `<rev>:<path>`, the file or directory the tree of the
//! revision has at the path, the tree itself for an empty path.
//! They compose from left to right: `HEAD~2^^~3`, `main@{u}~1:src`.

use alt_git_codec::{Commit, ObjectId, ObjectKind, Tree};

use crate::{RepoError, Repository};

/// What follows `@{` in a revision that asks for an upstream.
const UPSTREAM: [&str; 2] = ["@{u}", "@{upstream}"];

/// The number written at `bytes[*at..]`, which `at` is left behind: 1
/// where there is none, `None` where it is too large to be one.
fn count(bytes: &[u8], at: &mut usize) -> Option<usize> {
    let mut n: Option<usize> = None;
    while let Some(digit) = bytes.get(*at).filter(|b| b.is_ascii_digit()) {
        let more = n.unwrap_or(0).checked_mul(10)?;
        n = Some(more.checked_add(usize::from(digit - b'0'))?);
        *at += 1;
    }
    Some(n.unwrap_or(1))
}

impl Repository {
    /// The object `spec` names; `None` where it names nothing.
    pub fn rev_parse(&self, spec: &str) -> Result<Option<ObjectId>, RepoError> {
        // a path is all that follows the first colon
        if let Some((rev, path)) = spec.split_once(':') {
            let Some(named) = self.rev_parse(rev)? else {
                return Ok(None);
            };
            let Some(tree) = self.tree_of(named)? else {
                return Ok(None);
            };
            return self.entry_at(tree, path);
        }
        let split = spec.find(['~', '^']);
        let (base, suffix) = match split {
            None => (spec, ""),
            Some(i) => (&spec[..i], &spec[i..]),
        };
        let Some(mut oid) = self.resolve_base(base)? else {
            return Ok(None);
        };
        let bytes = suffix.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let op = bytes[i];
            i += 1;
            if op == b'^' && bytes.get(i) == Some(&b'{') {
                let Some(end) = suffix[i..].find('}') else {
                    return Ok(None);
                };
                let Some(peeled) = self.peel_to(oid, &suffix[i + 1..i + end])? else {
                    return Ok(None);
                };
                oid = peeled;
                i += end + 1;
                continue;
            }
            let Some(n) = count(bytes, &mut i) else {
                return Ok(None);
            };
            // Peel tags before reading the commit; ancestor operators
            // are commit-relative even when the user named a tag.
            let (peeled, object) = self.peel(oid)?;
            if object.kind != ObjectKind::Commit {
                return Ok(None);
            }
            let commit = Commit::parse(&object.data)?;
            let found = match op {
                b'~' => self.ancestor(peeled, commit, n)?,
                b'^' if n == 0 => Some(peeled),
                b'^' => commit.parents().nth(n - 1),
                _ => None,
            };
            let Some(found) = found else {
                return Ok(None);
            };
            oid = found;
        }
        Ok(Some(oid))
    }

    /// The commit `steps` first parents above `commit`, whose id is `at`.
    fn ancestor(
        &self,
        at: ObjectId,
        commit: Commit,
        steps: usize,
    ) -> Result<Option<ObjectId>, RepoError> {
        let (mut at, mut commit) = (at, commit);
        for _ in 0..steps {
            let Some(parent) = commit.parents().next() else {
                return Ok(None);
            };
            at = parent;
            commit = self.read_commit(&at)?;
        }
        Ok(Some(at))
    }

    /// `oid` peeled to an object of `kind`: `commit`, `tree`, or the
    /// empty word for whatever a tag names in the end.
    fn peel_to(&self, oid: ObjectId, kind: &str) -> Result<Option<ObjectId>, RepoError> {
        let (peeled, object) = self.peel(oid)?;
        Ok(match kind {
            "" => Some(peeled),
            "commit" => (object.kind == ObjectKind::Commit).then_some(peeled),
            "tree" => self.tree_of(peeled)?,
            "tag" => (self.kind_of(oid)? == Some(ObjectKind::Tag)).then_some(oid),
            "blob" => (object.kind == ObjectKind::Blob).then_some(peeled),
            "object" => Some(oid),
            _ => None,
        })
    }

    fn kind_of(&self, oid: ObjectId) -> Result<Option<ObjectKind>, RepoError> {
        Ok(self.read_object(&oid)?.map(|object| object.kind))
    }

    /// The tree `oid` names: its own where it is a commit or a tag of
    /// one, itself where it is a tree.
    fn tree_of(&self, oid: ObjectId) -> Result<Option<ObjectId>, RepoError> {
        let (peeled, object) = self.peel(oid)?;
        Ok(match object.kind {
            ObjectKind::Commit => Commit::parse(&object.data)?.tree(),
            ObjectKind::Tree => Some(peeled),
            _ => None,
        })
    }

    /// The entry of `tree` at `path`, the tree itself for the empty path.
    fn entry_at(&self, tree: ObjectId, path: &str) -> Result<Option<ObjectId>, RepoError> {
        let (mut mode, mut at) = (0o040000, tree);
        for part in path.split('/').filter(|part| !part.is_empty()) {
            if mode != 0o040000 {
                return Ok(None);
            }
            let object = self.read_object(&at)?.ok_or(RepoError::MissingObject(at))?;
            let Some(entry) = Tree::find(&object.data, self.algo()?, part.as_bytes())? else {
                return Ok(None);
            };
            (mode, at) = entry;
        }
        Ok(Some(at))
    }

    /// The remote-tracking branch `named` follows, as a full ref name:
    /// `named` is a branch, or a symbolic ref such as `HEAD` for the
    /// branch it is on. It is what `branch.<name>.remote` and
    /// `branch.<name>.merge` say; where they say nothing, the branch of
    /// the same name of the one remote that has such a branch.
    pub fn upstream_of(&self, named: &str) -> Result<Option<String>, RepoError> {
        let branch = match self.branch_of(named)? {
            Some(short) => short.to_string(),
            None => named
                .strip_prefix("refs/heads/")
                .unwrap_or(named)
                .to_owned(),
        };
        let config = self.config()?;
        let said = |key: &str| {
            let value = config.get_str("branch", Some(branch.as_bytes()), key);
            value.map(|value| value.to_string())
        };
        if let (Some(remote), Some(merge)) = (said("remote"), said("merge")) {
            let short = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
            return Ok(Some(match remote.as_str() {
                "." => merge.clone(),
                remote => format!("refs/remotes/{remote}/{short}"),
            }));
        }
        let follows = |name: &str| {
            let rest = name.strip_prefix("refs/remotes/")?;
            let (_, short) = rest.split_once('/')?;
            (short == branch).then(|| name.to_owned())
        };
        let refs = self.list_refs()?;
        let mut found = refs.iter().filter_map(|(name, ..)| follows(name));
        match (found.next(), found.next()) {
            (Some(only), None) => Ok(Some(only)),
            _ => Ok(None),
        }
    }

    /// The non-suffix portion of `rev_parse`: full hex, or a ref name
    /// through git's DWIM lookup order, `@` for `HEAD`, or the upstream
    /// of a branch.
    pub(crate) fn resolve_base(&self, spec: &str) -> Result<Option<ObjectId>, RepoError> {
        if spec.is_empty() {
            return Ok(None);
        }
        if spec == "@" {
            return self.resolve_ref("HEAD");
        }
        let braced = spec
            .strip_suffix('}')
            .and_then(|open| open.rsplit_once("@{"));
        if let Some((named, asked)) =
            braced.filter(|(_, asked)| !matches!(*asked, "u" | "upstream"))
        {
            return self.logged(spec, named, asked);
        }
        if let Some(named) = UPSTREAM.iter().find_map(|at| spec.strip_suffix(at)) {
            let named = if named.is_empty() || named == "@" {
                "HEAD"
            } else {
                named
            };
            return match self.upstream_of(named)? {
                Some(upstream) => self.resolve_ref(&upstream),
                None => Ok(None),
            };
        }
        if let Ok(oid) = ObjectId::from_hex(spec.as_bytes())
            && oid.algo() == self.algo()?
        {
            return Ok(Some(oid));
        }
        for candidate in [
            spec.to_owned(),
            format!("refs/{spec}"),
            format!("refs/tags/{spec}"),
            format!("refs/heads/{spec}"),
            format!("refs/remotes/{spec}"),
            format!("refs/remotes/{spec}/HEAD"),
        ] {
            if let Some(oid) = self.resolve_ref(&candidate)? {
                return Ok(Some(oid));
            }
        }
        // Short hex prefix as the final DWIM step: ≥ 4 hex chars, less
        // than the full hex length (the full-hex case is already handled
        // above), and unique within the odb. Returns
        // AmbiguousShortOid when multiple objects match, so the caller
        // surfaces a clear message instead of a silent miss.
        if spec.len() >= 4
            && spec.chars().all(|c| c.is_ascii_hexdigit())
            && spec.len() < self.algo()?.hex_len()
            && let Some(oid) = self.resolve_short_oid(spec)?
        {
            return Ok(Some(oid));
        }
        Ok(None)
    }
}
