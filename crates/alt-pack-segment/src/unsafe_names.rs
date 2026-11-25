//! The names a tree may not give its entries, as `git fsck` tells them: what a
//! checkout would write outside the directory of the tree, into the directory
//! of Git itself on some file system, or twice. A tree that only is not written
//! as Git writes it, out of order or with a mode given zeros before it, is
//! taken as it is.

/// What is wrong with the names of tree `data`, in the words of `git fsck`.
pub(crate) fn found(data: &[u8]) -> Option<&'static str> {
    let mut names = Vec::new();
    let mut rest = data;
    while let Some(space) = rest.iter().position(|byte| *byte == b' ') {
        rest = &rest[space + 1..];
        let nul = rest.iter().position(|byte| *byte == 0)?;
        let name = &rest[..nul];
        let wrong = match name {
            b"" => Some("emptyName: contains empty pathname"),
            b"." => Some("hasDot: contains '.'"),
            b".." => Some("hasDotdot: contains '..'"),
            _ if name.contains(&b'/') => Some("fullPathname: contains full pathnames"),
            _ if dot_git(name) => Some("hasDotgit: contains '.git'"),
            _ => None,
        };
        if wrong.is_some() {
            return wrong;
        }
        names.push(name);
        rest = rest.get(nul + 21..)?;
    }
    names.sort_unstable();
    let twice = names.windows(2).any(|two| two[0] == two[1]);
    twice.then_some("duplicateEntries: contains duplicate file entries")
}

/// `.git` as Git, NTFS or HFS+ reads the name
fn dot_git(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b".git") || ntfs(name) || hfs(name)
}

/// NTFS drops dots and spaces at the end of a name and reads what follows a
/// colon as a stream of the file; `git~1` is the short name of `.git`
fn ntfs(name: &[u8]) -> bool {
    let rest = if name.len() >= 4 && name[0] == b'.' && name[1..4].eq_ignore_ascii_case(b"git") {
        &name[4..]
    } else if name.len() >= 5 && name[..3].eq_ignore_ascii_case(b"git") && &name[3..5] == b"~1" {
        &name[5..]
    } else {
        return false;
    };
    let end = rest.iter().position(|byte| matches!(byte, b':' | b'\\'));
    rest[..end.unwrap_or(rest.len())]
        .iter()
        .all(|byte| matches!(byte, b'.' | b' '))
}

/// HFS+ leaves out code points that take no space, and folds case
fn hfs(name: &[u8]) -> bool {
    let Ok(name) = std::str::from_utf8(name) else {
        return false;
    };
    let ignored = |c: &char| matches!(*c as u32, 0x200c..=0x200f | 0x202a..=0x202e | 0x206a..=0x206f | 0xfeff);
    let mut left = name.chars().filter(|c| !ignored(c));
    let mut needle = ".git".chars();
    loop {
        match (left.next(), needle.next()) {
            (None, None) => return true,
            (Some(c), Some(n)) if c.to_ascii_lowercase() == n => {}
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(names: &[&[u8]]) -> Vec<u8> {
        let mut data = Vec::new();
        for name in names {
            data.extend_from_slice(b"100644 ");
            data.extend_from_slice(name);
            data.push(0);
            data.extend_from_slice(&[7; 20]);
        }
        data
    }

    #[test]
    fn names_git_fsck_refuses_are_found_and_others_are_not() {
        let dot_git: &[&[u8]] = &[
            b".git",
            b".GIT",
            b".Git. . ",
            b"git~1",
            b"GIT~1 ",
            b".git::$INDEX_ALLOCATION",
            b".g\xe2\x80\x8cit",
            b"\xef\xbb\xbf.GIT",
        ];
        for name in dot_git {
            assert_eq!(
                found(&tree(&[b"a", name])),
                Some("hasDotgit: contains '.git'"),
                "{name:?}"
            );
        }
        let taken: &[&[u8]] = &[
            b".gitignore",
            b".git-x",
            b"git~2",
            b"git",
            b".gi",
            b"x.git",
            b".g\xe2\x80\x8bit",
        ];
        for name in taken {
            assert_eq!(found(&tree(&[name])), None, "{name:?}");
        }
        assert_eq!(
            found(&tree(&[b""])),
            Some("emptyName: contains empty pathname")
        );
        assert_eq!(found(&tree(&[b"."])), Some("hasDot: contains '.'"));
        assert_eq!(found(&tree(&[b".."])), Some("hasDotdot: contains '..'"));
        assert_eq!(
            found(&tree(&[b"a/b"])),
            Some("fullPathname: contains full pathnames")
        );
        let twice = Some("duplicateEntries: contains duplicate file entries");
        assert_eq!(found(&tree(&[b"b", b"a", b"b"])), twice);
        // out of order, and a mode with a zero before it
        assert_eq!(found(&tree(&[b"b", b"a"])), None);
        let mut padded = b"040000 d\0".to_vec();
        padded.extend_from_slice(&[7; 20]);
        assert_eq!(found(&padded), None);
    }
}
