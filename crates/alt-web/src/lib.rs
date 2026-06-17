//! Public-facing HTTP API for `alt.golia.jp`. A handful of JSON endpoints
//! that surface project metadata (version, recent commits, basic stats)
//! plus a static landing fallback so the bare domain isn't a 404.
//!
//! Same stance as [`altd-server`]: synchronous, blocking, tiny_http —
//! the project keeps every server out of the async-runtime club so the
//! whole tree builds and tests without a tokio dependency tree. A
//! handful of slow requests on a marketing landing page is fine.

use std::io;
use std::path::{Path, PathBuf};

use alt_repo::Repository;

pub mod api;
pub mod router;

/// Read-only handle to the source `.alt` store the API surfaces.
///
/// `alt-web` does no writes. Holding the path (rather than an open
/// `Repository`) lets each request open the repo fresh, so a hot reload
/// of `.alt` on disk during deploys is seen on the next request.
#[derive(Debug, Clone)]
pub struct Source {
    alt_dir: PathBuf,
}

impl Source {
    /// Wrap a `.alt` directory. The path is not validated here; the first
    /// request that needs the repo will surface a clear `RepoOpen` error.
    pub fn new(alt_dir: impl Into<PathBuf>) -> Self {
        Self {
            alt_dir: alt_dir.into(),
        }
    }

    /// Open the repository fresh. A bad path → `Err(RepoOpen)`.
    pub fn open(&self) -> Result<Repository, ApiError> {
        Repository::discover(&self.alt_dir).map_err(|e| ApiError::RepoOpen(e.to_string()))
    }

    pub fn alt_dir(&self) -> &Path {
        &self.alt_dir
    }
}

/// Errors the API surface produces. Each maps to one HTTP status; the
/// router turns them into a stable JSON shape.
#[derive(Debug)]
pub enum ApiError {
    /// `Source::open` could not discover or read the `.alt` store. Surfaces
    /// as HTTP 503 — the service is up, the underlying data isn't ready.
    RepoOpen(String),
    /// A rev-walk or object read failed mid-request. HTTP 500.
    Internal(String),
}

impl ApiError {
    pub fn status_code(&self) -> u16 {
        match self {
            ApiError::RepoOpen(_) => 503,
            ApiError::Internal(_) => 500,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ApiError::RepoOpen(_) => "repo_unavailable",
            ApiError::Internal(_) => "internal",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            ApiError::RepoOpen(m) | ApiError::Internal(m) => m,
        }
    }
}

impl From<io::Error> for ApiError {
    fn from(e: io::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}
