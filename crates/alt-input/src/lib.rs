//! Complete, explicitly selected input trees. Hashes identify bytes and modes;
//! neither modification times nor version-control commits stand in for input.
mod filesystem;
mod manifest;
mod path;
mod platform;
mod serde_value;

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
    Json(serde_json::Error),
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
            Self::Json(e) => write!(f, "input encoding: {e}"),
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
            Self::Json(e) => Some(e),
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

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<alt_json::Error> for Error {
    fn from(e: alt_json::Error) -> Self {
        Self::Encoding(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

/// Domain-separated content identity over deterministic JSON. Structures use
/// fixed fields and maps use sorted keys; arbitrary JSON objects are sorted too.
pub fn identity<T: serde::Serialize>(domain: &str, value: &T) -> Result<String> {
    fn sorted(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => {
                let sorted: std::collections::BTreeMap<_, _> = object.into_iter().collect();
                serde_json::Value::Object(
                    sorted
                        .into_iter()
                        .map(|(key, value)| (key, sorted_value(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(sorted).collect())
            }
            value => value,
        }
    }
    // Keep the recursive object case independent of serde_json's map feature.
    fn sorted_value(value: serde_json::Value) -> serde_json::Value {
        sorted(value)
    }
    let bytes = serde_json::to_vec(&sorted_value(serde_json::to_value(value)?))?;
    Ok(digest(domain, &bytes))
}

/// [`identity`] for types written with alt-json; both give the same result
/// for the same value. A tree has its object keys sorted at every level.
pub fn identity_json<T: alt_json::ToJson>(domain: &str, value: &T) -> Result<String> {
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
