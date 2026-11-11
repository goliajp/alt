//! Byte strings: text that is conventionally UTF-8 but is not required to
//! be, such as paths, ref names and commit messages read from a repository.
//!
//! [`BString`] owns its bytes and [`BStr`] borrows them; both print as text
//! instead of as a list of integers. [`ByteSlice`] adds the string-like
//! operations to plain `[u8]`.

mod block;
mod bstr;
mod bstring;
mod cmp;
mod ext_slice;
mod fmt;

pub use bstr::BStr;
pub use bstring::BString;
pub use ext_slice::{ByteSlice, Lines};
