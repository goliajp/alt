//! Hash functions written against the hardware, with no dependencies.
//!
//! Each algorithm picks the fastest implementation the running CPU supports
//! and keeps a portable one that the others are tested against.

pub mod sha1;
mod sha1_many;

pub use sha1::{Sha1, sha1};
pub use sha1_many::{Parts, sha1_many};
