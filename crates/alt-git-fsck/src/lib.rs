//! What `git fsck` refuses in the trees and the blobs a push brings, as a Git
//! server that checks what it receives refuses it, and which of that a push
//! brings anew.
//!
//! A tree is refused for the names of its entries: a name a checkout would
//! write outside the directory of the tree or into the directory of Git (or
//! of alt), a name twice, `.gitmodules` as a symbolic link. A blob is refused
//! for what it holds where a tree names it `.gitmodules` or `.gitattributes`.
//!
//! What a repository already holds is not refused again: a name a tree had at
//! the same place in a parent of the commit, or a file it named there with
//! the same id, is no fault of the push. The trees of the commits a push
//! brings are walked where they differ from those of their parents, and each
//! fault is told of at the places it is come to.

mod blob;
mod tree;
mod url;
mod walk;

pub use blob::{ATTRIBUTES_LARGEST, Blob, MODULES_LARGEST, blob_fault};
pub use tree::{Entry, Fault, File, Flagged, Special, flagged};
pub use walk::{Commit, Files, Trees, first_new};

/// the mode of an entry that names a tree
pub const TREE: u32 = 0o040000;
/// the mode of an entry that is a symbolic link
pub const SYMLINK: u32 = 0o120000;
