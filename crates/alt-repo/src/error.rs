//! What reading a repository fails with.

use std::path::PathBuf;

use alt_git_codec::ObjectId;

#[derive(Debug)]
pub enum RepoError {
    NotARepository(PathBuf),
    MissingObject(ObjectId),
    Unsupported(&'static str),
    Format(&'static str),
    /// A short oid prefix matched more than one object — the caller has
    /// to commit to a longer prefix to disambiguate. Carries up to four
    /// example matches so the error message can list them.
    AmbiguousShortOid {
        prefix: String,
        matches: Vec<ObjectId>,
    },
    /// the log of a ref was asked for a move it does not go back to
    LogShort {
        name: String,
        has: usize,
    },
    /// the log of a ref was asked by something that is no number
    LogByDate(String),
    Io(std::io::Error),
    Loose(alt_git_codec::LooseError),
    Object(alt_git_codec::ObjectParseError),
    Pack(alt_git_pack::PackError),
    Refs(alt_git_refs::RefError),
    Config(alt_git_config::ConfigError),
    Odb(alt_odb::OdbError),
    NativeRefs(alt_refs::RefError),
}

impl std::fmt::Display for RepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARepository(path) => write!(
                f,
                "not a git repository (or any parent): {}",
                path.display()
            ),
            Self::MissingObject(oid) => write!(f, "object {oid} is referenced but missing"),
            Self::Unsupported(what) => write!(f, "{what} are not supported yet"),
            Self::Format(what) => write!(f, "repository format: {what}"),
            Self::AmbiguousShortOid { prefix, matches } => write!(
                f,
                "short oid '{prefix}' is ambiguous: matches {} objects",
                matches.len()
            ),
            Self::LogShort { name, has } => {
                write!(f, "log for '{name}' only has {has} entries")
            }
            Self::LogByDate(spec) => write!(
                f,
                "'{spec}': the log of a ref is asked by number, as in `@{{1}}`; a date is not read there"
            ),
            Self::Io(_) => f.write_str("io"),
            Self::Loose(e) => std::fmt::Display::fmt(e, f),
            Self::Object(e) => std::fmt::Display::fmt(e, f),
            Self::Pack(e) => std::fmt::Display::fmt(e, f),
            Self::Refs(e) => std::fmt::Display::fmt(e, f),
            Self::Config(e) => std::fmt::Display::fmt(e, f),
            Self::Odb(e) => std::fmt::Display::fmt(e, f),
            Self::NativeRefs(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for RepoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Loose(e) => e.source(),
            Self::Object(e) => e.source(),
            Self::Pack(e) => e.source(),
            Self::Refs(e) => e.source(),
            Self::Config(e) => e.source(),
            Self::Odb(e) => e.source(),
            Self::NativeRefs(e) => e.source(),
            Self::NotARepository(_)
            | Self::MissingObject(_)
            | Self::Unsupported(_)
            | Self::Format(_)
            | Self::AmbiguousShortOid { .. }
            | Self::LogShort { .. }
            | Self::LogByDate(_) => None,
        }
    }
}

impl From<std::io::Error> for RepoError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<alt_git_codec::LooseError> for RepoError {
    fn from(e: alt_git_codec::LooseError) -> Self {
        Self::Loose(e)
    }
}

impl From<alt_git_codec::ObjectParseError> for RepoError {
    fn from(e: alt_git_codec::ObjectParseError) -> Self {
        Self::Object(e)
    }
}

impl From<alt_git_pack::PackError> for RepoError {
    fn from(e: alt_git_pack::PackError) -> Self {
        Self::Pack(e)
    }
}

impl From<alt_git_refs::RefError> for RepoError {
    fn from(e: alt_git_refs::RefError) -> Self {
        Self::Refs(e)
    }
}

impl From<alt_git_config::ConfigError> for RepoError {
    fn from(e: alt_git_config::ConfigError) -> Self {
        Self::Config(e)
    }
}

impl From<alt_odb::OdbError> for RepoError {
    fn from(e: alt_odb::OdbError) -> Self {
        Self::Odb(e)
    }
}

impl From<alt_oplog::OpLogError> for RepoError {
    fn from(e: alt_oplog::OpLogError) -> Self {
        Self::NativeRefs(e.into())
    }
}

impl From<alt_refs::RefError> for RepoError {
    fn from(e: alt_refs::RefError) -> Self {
        Self::NativeRefs(e)
    }
}
