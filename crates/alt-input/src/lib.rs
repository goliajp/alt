//! Complete, explicitly selected input trees. Hashes identify bytes and modes;
//! neither modification times nor version-control commits stand in for input.
mod filesystem;
mod manifest;
mod path;
mod platform;

pub use filesystem::Root;
pub use manifest::Tree;
pub use manifest::{
    Context, Entry, EntryKind, Environment, ExternalDependency, InputPlan, InputRole, InputRoot,
    Manifest, Node, ToolIdentity, scan,
};
pub use path::{checked_path, hex, unhex};
pub use platform::check_execution_platform;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Encoding(alt_json::Error),
    Path {
        reason: &'static str,
        bytes_hex: String,
    },
    Unsupported(String),
    Changed(String),
    Integrity(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "input I/O: {e}"),
            Self::Encoding(e) => write!(f, "input encoding: {e}"),
            Self::Path { reason, bytes_hex } => {
                write!(f, "unsupported input path ({reason}); bytes={bytes_hex}")
            }
            Self::Unsupported(v) => write!(f, "unsupported input: {v}"),
            Self::Changed(v) => write!(f, "input changed while being captured: {v}"),
            Self::Integrity(v) => write!(f, "input integrity: {v}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Encoding(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<alt_json::Error> for Error {
    fn from(e: alt_json::Error) -> Self {
        Self::Encoding(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

/// Domain-separated content identity over deterministic JSON: the tree of
/// the value, whose object keys are sorted at every level.
pub fn identity<T: alt_json::ToJson>(domain: &str, value: &T) -> Result<String> {
    let bytes = alt_json::to_vec(&alt_json::to_value(value)?)?;
    Ok(digest(domain, &bytes))
}

fn digest(domain: &str, bytes: &[u8]) -> String {
    let mut hash = alt_hash::blake3::Hasher::new();
    hash.update(b"alt-input-v1\0");
    hash.update(domain.as_bytes());
    hash.update(b"\0");
    hash.update(bytes);
    hash.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests;
