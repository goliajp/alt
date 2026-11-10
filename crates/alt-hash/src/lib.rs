//! Hash functions written against the hardware, with no dependencies.
//!
//! Each algorithm picks the fastest implementation the running CPU supports
//! and keeps a portable one that the others are tested against.

mod adler32;
pub mod blake3;
mod cache;
pub mod crc32;
pub mod sha1;
mod sha1_many;
pub mod sha256;

pub use adler32::adler32;
pub use crc32::{Crc32, crc32};
pub use sha1::{Sha1, sha1};
pub use sha1_many::{Parts, sha1_many};
pub use sha256::{Sha256, sha256};
