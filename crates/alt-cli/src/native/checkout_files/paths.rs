//! Which paths a checkout may write to.

use super::*;

/// Whether a file system may take `name`, one step of a path, for the
/// control directory of a repository, `.alt` or `.git`: in another case,
/// with code points HFS+ passes over within it, with the dots and blanks
/// NTFS drops from its end, as the short name NTFS gives it, or with a
/// stream of the directory named behind a colon. As git has it for
/// `.git`, on every system: a repository is checked out on others too.
pub(in crate::native) fn names_control_directory(name: &[u8]) -> bool {
    // the code points HFS+ ignores, as UTF-8: U+200C to U+200F, U+202A
    // to U+202E, U+206A to U+206F and U+FEFF
    let ignored = |at: &[u8]| {
        matches!(
            at,
            [0xe2, 0x80, 0x8c..=0x8f | 0xaa..=0xae, ..]
                | [0xe2, 0x81, 0xaa..=0xaf, ..]
                | [0xef, 0xbb, 0xbf, ..]
        )
    };
    let mut seen = Vec::with_capacity(name.len());
    let mut at = 0;
    while at < name.len() {
        match ignored(&name[at..]) {
            true => at += 3,
            false => {
                seen.push(name[at].to_ascii_lowercase());
                at += 1;
            }
        }
    }
    let end = seen.iter().position(|b| matches!(b, b':' | b'\\'));
    let mut head = &seen[..end.unwrap_or(seen.len())];
    while let [rest @ .., b' ' | b'.'] = head {
        head = rest;
    }
    matches!(head, b".alt" | b".git" | b"alt~1" | b"git~1")
}

/// Refuses a path a command is to write at or to record where one of its
/// steps may be taken for a control directory.
pub(in crate::native) fn ensure_no_control_directory(path: &[u8]) -> Res<()> {
    let mut steps = path.split(|b| *b == b'/');
    let asked = steps.clone().next() == Some(alt_worktree::NAMES_ASKED.as_bytes());
    if asked || steps.any(names_control_directory) {
        let path = String::from_utf8_lossy(path);
        return Err(said::refused(
            &format!("error: invalid path '{path}'\n"),
            128,
        ));
    }
    Ok(())
}

/// Removes `path` when it is a directory with nothing but empty directories
/// inside; a file anywhere in it is an error.
pub(super) fn remove_empty_directories(path: &Path) -> Res<()> {
    if !path.symlink_metadata().is_ok_and(|m| m.is_dir()) {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            return Err("checkout target is occupied by a directory".into());
        }
        remove_empty_directories(&entry.path())?;
    }
    std::fs::remove_dir(path)?;
    Ok(())
}

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

    /// What stands above `path` right now: every step a directory of the
    /// working tree itself, or a step that is not there yet, or `Err`
    /// with the step that is a file or a symlink.
    pub(super) fn steps_above(&self, path: &[u8]) -> Res<Result<bool, BString>> {
        for at in (0..path.len()).filter(|at| path[*at] == b'/') {
            let step = BString::from(&path[..at]);
            match self.abs(&step)?.symlink_metadata() {
                Ok(found) if found.is_dir() => {}
                Ok(_) => return Ok(Err(step)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ok(false)),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Ok(true))
    }

    /// Whether a file can be put at `path` now: no step above it is a
    /// file or a symlink.
    pub(super) fn may_stand(&self, path: &[u8]) -> Res<bool> {
        Ok(self.steps_above(path)?.is_ok())
    }

    /// Whether every step above `path` is a directory of the working
    /// tree; refused where one is a file or a symlink, which nothing is
    /// reached through.
    pub(super) fn beneath_directories(&self, path: &[u8]) -> Res<bool> {
        self.steps_above(path)?.map_err(|step| {
            let path = BString::from(path);
            format!("'{step}', above '{path}', is not a directory of the working tree").into()
        })
    }

    /// Makes the directories above `path` that are not there, one step
    /// at a time: none is made, and nothing is written, past a step that
    /// is a file or a symlink.
    pub(super) fn make_directories_above(&self, path: &[u8]) -> Res<()> {
        while !self.beneath_directories(path)? {
            let missing = (0..path.len())
                .filter(|at| path[*at] == b'/')
                .map(|at| self.abs(&BString::from(&path[..at])))
                .find(|step| {
                    !step
                        .as_ref()
                        .is_ok_and(|step| step.symlink_metadata().is_ok())
                });
            std::fs::create_dir(missing.expect("a step is not there")?)?;
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::names_control_directory as taken;

    #[test]
    fn what_a_file_system_takes_for_a_control_directory_is_told() {
        for name in [
            ".alt",
            ".git",
            ".ALT",
            ".Git",
            ".alt.",
            ".alt ",
            ".alt . .",
            "ALT~1",
            "git~1",
            ".alt::$INDEX_ALLOCATION",
            ".git:stream",
            ".alt\\x",
            ".a\u{200c}lt",
            "\u{feff}.git",
            ".al\u{202e}t\u{206f}",
            ".G\u{200d}IT ",
        ] {
            assert!(taken(name.as_bytes()), "{name:?}");
        }
        for name in [
            ".alto",
            "alt",
            ".altx.",
            "alt~2",
            ".gitignore",
            ".git~1",
            "a.alt",
            ".al",
            "..alt",
            ".a\u{200b}lt",
            "x:.alt",
        ] {
            assert!(!taken(name.as_bytes()), "{name:?}");
        }
    }
}
