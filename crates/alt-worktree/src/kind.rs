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
