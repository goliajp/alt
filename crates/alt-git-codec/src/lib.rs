//! Git object model codec.
//!
//! Parses and serializes git's core object types (blob, tree, commit, tag)
//! and computes their object ids (SHA-1 / SHA-256). This crate is
//! business-agnostic: it knows the git on-disk object format and nothing
//! about alt.

mod kind;
mod loose;
mod oid;

pub use kind::{ObjectKind, ParseKindError};
pub use loose::{LooseError, LooseStore, RawObject};
pub use oid::{HashAlgo, ObjectId, ParseOidError};
