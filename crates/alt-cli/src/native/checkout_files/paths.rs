//! Which paths a checkout may write to.

use super::*;

impl NativeRepo<'_> {
    /// Checks that `bytes` names a path inside the working tree whose
    /// parents are real directories. A parent that is a file or a symlink
    /// is accepted only when the operation owns it, since it is then
    /// removed before anything is written beneath; the result says whether
    /// there is such a parent.
    pub(in crate::native) fn validate_path_among(
        &self,
        bytes: &BString,
        owned: &dyn Fn(&[u8]) -> bool,
    ) -> Res<bool> {
        let path = bytes.as_slice();
        let no_name = |c: &[u8]| c.is_empty() || matches!(c, b"." | b".." | b".alt" | b".git");
        // an absolute path begins with a name that is empty
        if path.is_empty() || path.split(|b| *b == b'/').any(no_name) {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                "unsafe recovery path",
            )
            .into());
        }
        let mut beneath_owned = false;
        // outermost parent first: nothing past a symlink is looked at
        let mut end = 0;
        while let Some(at) = path[end..].iter().position(|b| *b == b'/') {
            end += at;
            let parent = &path[..end];
            end += 1;
            match self.abs(&BString::from(parent))?.symlink_metadata() {
                Ok(m) if m.is_dir() => {}
                Ok(_) if owned(parent) => {
                    beneath_owned = true;
                    break;
                }
                Ok(_) => {
                    return Err(crate::machine::Failure::refusal(
                        "precondition_failed",
                        "recovery path traverses an unsafe parent",
                    )
                    .into());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(beneath_owned)
    }

    /// Newly created or removed directory entries must reach disk too,
    /// all the way back to the existing workspace root: syncs every
    /// directory above `abs` that is there.
    pub(super) fn sync_parents(&self, abs: &Path) -> Res<()> {
        let mut parent = abs.parent();
        while let Some(path) = parent {
            match std::fs::File::open(path) {
                Ok(dir) => dir.sync_all()?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if path == self.root {
                break;
            }
            parent = path.parent();
        }
        Ok(())
    }
}
