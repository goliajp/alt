//! What the searches with one pattern keep between them.

use crate::{backtrack, dfa, pike};

/// Storage the searches with one pattern reuse. A thread that searches
/// often keeps its own and passes it to the `_with` methods; the others
/// share one inside the pattern, behind a lock.
pub struct Cache {
    /// for a match that begins anywhere
    pub(crate) scan: dfa::Cache,
    /// for a match that begins where the search does
    pub(crate) fixed: dfa::Cache,
    /// for the rest of a match whose beginning was found otherwise
    pub(crate) tail: dfa::Cache,
    pub(crate) pike: pike::Cache,
    pub(crate) backtrack: backtrack::Cache,
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            scan: dfa::Cache::new(true),
            fixed: dfa::Cache::new(false),
            tail: dfa::Cache::new(false),
            pike: pike::Cache::default(),
            backtrack: backtrack::Cache::default(),
        }
    }
}
