//! Short forms of object ids: printing an id with as few digits as keep it
//! apart from the rest of the store.

use alt_git_codec::ObjectId;

use crate::{NativeOdb, OdbError};

/// Abbreviates object ids the way git does by default: the length grows
/// with the size of the store, and an id that another object shares that
/// many digits with gets as many more as tell them apart.
pub struct Abbrev<'a> {
    odb: &'a NativeOdb,
    min: usize,
}

impl NativeOdb {
    pub fn abbrev(&self) -> Result<Abbrev<'_>, OdbError> {
        Ok(Abbrev {
            odb: self,
            min: Abbrev::default_len(self.len()?),
        })
    }
}

impl Abbrev<'_> {
    /// The digits git starts from in a store of `objects` objects: enough
    /// for ids to be expected to stay apart, and never fewer than seven.
    pub fn default_len(objects: usize) -> usize {
        let bits = (usize::BITS - objects.leading_zeros()) as usize;
        bits.div_ceil(2).max(7)
    }

    /// The same, starting from `len` digits in place of the default.
    pub fn from_len(mut self, len: usize) -> Self {
        self.min = len;
        self
    }

    /// The default number of digits for this store.
    pub fn min_len(&self) -> usize {
        self.min
    }

    /// `oid` cut to the default length, or to one digit more than it
    /// shares with any other object when that is longer. The id need not
    /// be in the store.
    pub fn of(&self, oid: &ObjectId) -> Result<String, OdbError> {
        let hex = oid.to_string();
        let mut len = self.min.min(hex.len());
        // one more than the object itself: any other id under the prefix
        while len < hex.len() {
            let under = self.odb.odb()?.map.by_prefix(&hex[..len], 2)?;
            if under.iter().all(|entry| entry.git == *oid) {
                break;
            }
            len += 1;
        }
        Ok(hex[..len].to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starting_length_grows_with_the_store() {
        assert_eq!(Abbrev::default_len(0), 7);
        // the cargo and git repositories, where git prints 9 and 10 digits
        assert_eq!(Abbrev::default_len(185_011), 9);
        assert_eq!(Abbrev::default_len(402_230), 10);
    }
}
