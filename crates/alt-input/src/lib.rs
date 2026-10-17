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

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("input I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("input encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported input path ({reason}); bytes={bytes_hex}")]
    Path {
        reason: &'static str,
        bytes_hex: String,
    },
    #[error("unsupported input: {0}")]
    Unsupported(String),
    #[error("input changed while being captured: {0}")]
    Changed(String),
    #[error("input integrity: {0}")]
    Integrity(String),
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
    let mut hash = blake3::Hasher::new();
    hash.update(b"alt-input-v1\0");
    hash.update(domain.as_bytes());
    hash.update(b"\0");
    hash.update(&bytes);
    Ok(hash.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests;
