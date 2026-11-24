//! What happened to a path between two trees.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// one of a file, a symlink and a submodule where another was
    TypeChanged,
}

impl ChangeKind {
    /// What it is for an entry of mode `old` to give way to one of mode
    /// `new` at the same path.
    pub fn between(old: u32, new: u32) -> ChangeKind {
        const TYPE: u32 = 0o170000;
        match old & TYPE == new & TYPE {
            true => ChangeKind::Modified,
            false => ChangeKind::TypeChanged,
        }
    }
}

/// The directory a command asks the file system in which names are one
/// name, at the root of a working tree whose `.alt` is no directory to
/// hold it. It is no part of the tree.
pub const NAMES_ASKED: &str = ".alt-names-asked";
