//! Shared capability syntax; only an authority adapter can grant it.
use crate::{Error, Result};

#[derive(
    Clone, Debug, serde::Serialize, serde::Deserialize, alt_json::ToJson, alt_json::FromJson,
)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct Grant {
    pub refs: RefSelector,
    pub create: bool,
    pub update: bool,
    pub delete: bool,
    pub force: bool,
    /// None means all paths; Some(empty) grants no changed paths. Paths are
    /// raw Git bytes, matched at component boundaries, never lossy UTF-8.
    pub paths: Option<Vec<Vec<u8>>>,
}
#[derive(
    Clone, Debug, serde::Serialize, serde::Deserialize, alt_json::ToJson, alt_json::FromJson,
)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub enum RefSelector {
    Exact(String),
    /// Must end in '/', e.g. refs/heads/. Never a free-form glob.
    Namespace(String),
}
impl RefSelector {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Exact(value) => crate::validate_ref_name(value)?,
            Self::Namespace(value) => {
                if value == "refs/" {
                    return Ok(());
                }
                let prefix = value
                    .strip_suffix('/')
                    .ok_or(Error::Invalid("invalid ref namespace policy"))?;
                crate::validate_ref_name(prefix)?;
            }
        }
        Ok(())
    }
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(value) => value == name,
            Self::Namespace(value) => {
                value.starts_with("refs/") && value.ends_with('/') && name.starts_with(value)
            }
        }
    }
}
impl Grant {
    pub fn validate(&self) -> Result<()> {
        self.refs.validate()?;
        if let Some(paths) = &self.paths
            && paths.iter().any(|p| !valid_path_prefix(p))
        {
            return Err(Error::Invalid("invalid path grant"));
        }
        Ok(())
    }
}
fn valid_path_prefix(prefix: &[u8]) -> bool {
    !prefix.is_empty()
        && prefix.len() <= 4096
        && !prefix.contains(&0)
        && !prefix
            .split(|b| *b == b'/')
            .any(|p| p.is_empty() || p == b"." || p == b"..")
}
