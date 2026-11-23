//! The kinds of failure a command has said in its own words, as git says
//! them: nothing but the request is added to what it wrote.

/// The kind of failure `add` ends with when it left paths alone that an
/// ignore rule hides: its details name them under `ignored_paths`.
pub const PATHS_IGNORED: &str = "paths_ignored";

/// The kind of failure `add -u` ends with when a path given names a file
/// the index has not, which it has said as git says it.
pub const PATHS_UNKNOWN: &str = "paths_unknown";

/// The kind of failure `add` ends with when it could not store a path
/// and has said which, as git says it.
pub const PATHS_FAILED: &str = "paths_failed";

/// The kind of failure of a command that refused to do anything and has
/// said why as git says it.
pub const REFUSED_AS_SAID: &str = "refused_as_said";
