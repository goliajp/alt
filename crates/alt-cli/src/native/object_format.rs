//! The hash algorithm of a store's object ids.

use super::*;

impl Store {
    /// The algorithm of the git repository the store was imported from,
    /// else sha1. It is in a config file of its own, read when a command
    /// first hashes or parses something: most read commands never do.
    pub fn algo(&self) -> Res<HashAlgo> {
        if let Some(algo) = self.algo.get() {
            return Ok(*algo);
        }
        let algo = alt_repo::native_object_format(&self.alt_dir)?;
        Ok(*self.algo.get_or_init(|| algo))
    }
}
