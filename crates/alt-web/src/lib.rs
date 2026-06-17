//! Public-facing HTTP API for `alt.golia.jp`. Hand-encoded JSON over
//! plain HTTP — a marketing landing API plus a thin browse API that the
//! product page calls (repo list, refs, commit log, commit detail with
//! diff, tree, blob).
//!
//! Same stance as [`altd-server`]: synchronous, blocking, tiny_http —
//! the project keeps every server out of the async-runtime club so the
//! whole tree builds and tests without a tokio dependency tree. A
//! handful of slow requests on a marketing landing page is fine.
//!
//! ## Multi-repo
//!
//! The server is rooted at a directory containing one subdirectory per
//! repository, with each subdir holding a `.alt` store (same layout
//! altd-server uses for its `ALT_SERVER_ROOT`). The `/api/repos` family
//! discovers and dispatches by the first path segment.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use alt_repo::Repository;

pub mod api;
pub mod ooxml;
pub mod router;

/// Root of the multi-repo layout. Each top-level subdirectory under
/// `root` is one repository whose store lives at `<sub>/.alt`.
#[derive(Debug, Clone)]
pub struct MultiRepo {
    root: PathBuf,
}

impl MultiRepo {
    /// Wrap a root directory. Not validated; bad paths surface on the
    /// first list / open call.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// List every subdirectory under `root` that contains a `.alt` store,
    /// sorted by name. Subdirectories without `.alt` are silently skipped
    /// — the layout convention is exactly one repo per dir.
    pub fn list(&self) -> Result<Vec<String>, ApiError> {
        let entries = fs::read_dir(&self.root)
            .map_err(|e| ApiError::Internal(format!("read root {}: {e}", self.root.display())))?;
        let mut names = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if name_str.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.join(".alt").is_dir() {
                names.push(name_str.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Resolve a repo by name. Returns `RepoNotFound` for an unknown name
    /// (404), `RepoOpen` for a present-but-broken store (503).
    pub fn open(&self, name: &str) -> Result<Repository, ApiError> {
        if !is_safe_name(name) {
            return Err(ApiError::RepoNotFound(name.to_string()));
        }
        let path = self.root.join(name);
        if !path.join(".alt").is_dir() {
            return Err(ApiError::RepoNotFound(name.to_string()));
        }
        Repository::discover(&path).map_err(|e| ApiError::RepoOpen(e.to_string()))
    }
}

/// Names are URL path segments and must not be allowed to escape the
/// root (`..`) or carry separators (`/`, `\`, NUL). This mirrors what
/// altd-server's `resolve_repo` enforces — same model, same defence.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name != ".."
        && name != "."
}

/// Errors the API surface produces. Each maps to one HTTP status; the
/// router turns them into a stable JSON shape.
#[derive(Debug)]
pub enum ApiError {
    /// Repo name does not exist under the root. HTTP 404.
    RepoNotFound(String),
    /// `MultiRepo::open` found the dir but could not read the `.alt`
    /// store. Surfaces as HTTP 503 — the repo is named, the data isn't.
    RepoOpen(String),
    /// An object lookup against a present repo returned `None` where
    /// the caller asked for a specific oid / ref. HTTP 404.
    NotFound(String),
    /// A rev-walk or object read failed mid-request. HTTP 500.
    Internal(String),
}

impl ApiError {
    pub fn status_code(&self) -> u16 {
        match self {
            ApiError::RepoNotFound(_) | ApiError::NotFound(_) => 404,
            ApiError::RepoOpen(_) => 503,
            ApiError::Internal(_) => 500,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ApiError::RepoNotFound(_) => "repo_not_found",
            ApiError::RepoOpen(_) => "repo_unavailable",
            ApiError::NotFound(_) => "not_found",
            ApiError::Internal(_) => "internal",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            ApiError::RepoNotFound(m)
            | ApiError::RepoOpen(m)
            | ApiError::NotFound(m)
            | ApiError::Internal(m) => m,
        }
    }
}

impl From<io::Error> for ApiError {
    fn from(e: io::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_safe_name_rejects_path_separators_and_traversal() {
        assert!(is_safe_name("alt"));
        assert!(is_safe_name("project-name"));
        assert!(!is_safe_name(""));
        assert!(!is_safe_name("a/b"));
        assert!(!is_safe_name("a\\b"));
        assert!(!is_safe_name(".."));
        assert!(!is_safe_name("."));
        assert!(!is_safe_name("a\0b"));
    }

    #[test]
    fn list_finds_dot_alt_dirs_skips_others() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("repo-a/.alt")).unwrap();
        std::fs::create_dir_all(root.join("repo-b/.alt")).unwrap();
        std::fs::create_dir_all(root.join("bare-dir-no-alt")).unwrap();
        std::fs::create_dir_all(root.join(".hidden/.alt")).unwrap();
        std::fs::write(root.join("loose-file"), "x").unwrap();

        let m = MultiRepo::new(root);
        let names = m.list().unwrap();
        assert_eq!(names, vec!["repo-a".to_string(), "repo-b".to_string()]);
    }

    #[test]
    fn open_reports_repo_not_found_for_missing_and_unsafe_names() {
        let tmp = tempfile::tempdir().unwrap();
        let m = MultiRepo::new(tmp.path());
        // Repository doesn't impl Debug, so unwrap_err panics format —
        // pattern-match instead.
        match m.open("nope") {
            Err(ApiError::RepoNotFound(_)) => {}
            other => panic!("expected RepoNotFound for nope, got {:?}", other.err()),
        }
        match m.open("../escape") {
            Err(ApiError::RepoNotFound(_)) => {}
            other => panic!("expected RepoNotFound for traversal, got {:?}", other.err()),
        }
    }
}
