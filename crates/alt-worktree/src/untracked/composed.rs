//! `core.precomposeUnicode`: a file system that lists a name decomposed
//! reaches the file by the composed name as well, and git takes every
//! name it reads from a directory put together again there.

use crate::fsdir::Listing;

pub use alt_precompose::composed;

impl Listing {
    /// [`add`](Self::add), with the name composed where `precompose`
    /// asks for it.
    pub(crate) fn take(
        &mut self,
        name: &[u8],
        d_type: u8,
        precompose: bool,
        unknown: &mut Vec<usize>,
    ) {
        let text = &name[..name.len() - 1];
        match composed(text).filter(|_| precompose) {
            Some(mut name) => {
                name.push(0);
                self.add(&name, d_type, unknown)
            }
            None => self.add(name, d_type, unknown),
        }
    }
}
