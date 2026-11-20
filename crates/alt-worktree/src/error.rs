//! What reading and writing the trees of a working tree fails with.

use alt_bytes::BString;
use alt_git_codec::ObjectId;
use alt_odb::OdbError;

#[derive(Debug)]
pub enum WorktreeError {
    Io(std::io::Error),
    Odb(OdbError),
    Object(alt_git_codec::ObjectParseError),
    NotATree(ObjectId),
    /// a tree was to be written with two entries of this name
    Duplicate(BString),
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::Odb(e) => std::fmt::Display::fmt(e, f),
            Self::Object(e) => std::fmt::Display::fmt(e, f),
            Self::NotATree(v) => write!(f, "object {v} is not a tree"),
            Self::Duplicate(name) => write!(f, "tree would hold '{name}' twice"),
        }
    }
}

impl std::error::Error for WorktreeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Odb(e) => e.source(),
            Self::Object(e) => e.source(),
            _ => None,
        }
    }
}

impl From<std::io::Error> for WorktreeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<OdbError> for WorktreeError {
    fn from(e: OdbError) -> Self {
        Self::Odb(e)
    }
}

impl From<alt_git_codec::ObjectParseError> for WorktreeError {
    fn from(e: alt_git_codec::ObjectParseError) -> Self {
        Self::Object(e)
    }
}
