//! The names a tree may not give an entry, as `git fsck` tells them: what a
//! checkout would write outside the directory of the tree, or into the
//! directory of Git, or of alt, on some file system.

/// What is wrong with `name` as the name of an entry, in the words of
/// `git fsck`; `.alt` is told as `.git` is.
pub fn unsafe_name(name: &[u8]) -> Option<&'static str> {
    match name {
        b"" => Some("emptyName: contains empty pathname"),
        b"." => Some("hasDot: contains '.'"),
        b".." => Some("hasDotdot: contains '..'"),
        _ if name.contains(&b'/') => Some("fullPathname: contains full pathnames"),
        _ if read_as(name, b"git") => Some("hasDotgit: contains '.git'"),
        _ if read_as(name, b"alt") => Some("hasDotalt: contains '.alt'"),
        _ => None,
    }
}

/// `.` and `word` as Git, NTFS or HFS+ reads the name; NTFS reads what
/// follows each backslash as a name of its own
fn read_as(name: &[u8], word: &[u8; 3]) -> bool {
    let dotted = [&b"."[..], word].concat();
    let mut parts = (name.iter().enumerate()).filter(|(_, byte)| **byte == b'\\');
    name.eq_ignore_ascii_case(&dotted)
        || ntfs(name, word)
        || hfs(name, &dotted)
        || parts.any(|(at, _)| ntfs(&name[at + 1..], word))
}

/// NTFS drops dots and spaces at the end of a name and reads what follows a
/// colon as a stream of the file; `<word>~1` is the short name of `.<word>`
fn ntfs(name: &[u8], word: &[u8; 3]) -> bool {
    let rest = if name.len() >= 4 && name[0] == b'.' && name[1..4].eq_ignore_ascii_case(word) {
        &name[4..]
    } else if name.len() >= 5 && name[..3].eq_ignore_ascii_case(word) && &name[3..5] == b"~1" {
        &name[5..]
    } else {
        return false;
    };
    let end = rest.iter().position(|byte| matches!(byte, b':' | b'\\'));
    rest[..end.unwrap_or(rest.len())]
        .iter()
        .all(|byte| matches!(byte, b'.' | b' '))
}

/// HFS+ leaves out code points that take no space, and folds case; as Git
/// reads a name, one ends where its bytes stop being UTF-8
fn hfs(name: &[u8], dotted: &[u8]) -> bool {
    let read = name.utf8_chunks().next().map_or("", |chunk| chunk.valid());
    let ignored = |c: &char| matches!(*c as u32, 0x200c..=0x200f | 0x202a..=0x202e | 0x206a..=0x206f | 0xfeff);
    let mut left = read.chars().filter(|c| !ignored(c));
    let mut needle = dotted.iter().map(|byte| char::from(*byte));
    loop {
        match (left.next(), needle.next()) {
            (None, None) => return true,
            (Some(c), Some(n)) if c.to_ascii_lowercase() == n => {}
            _ => return false,
        }
    }
}

/// Whether `name` is read as `.<file>` by Git, by HFS+ or by NTFS, whose short
/// names for it begin with `short` (as `git fsck` has them, `gi7eba` for
/// `gitmodules`). With `backslashes`, also what follows a backslash, as Git
/// reads a name for `.gitmodules`.
pub fn dot_file(name: &[u8], file: &str, short: &[u8; 6], backslashes: bool) -> bool {
    let dotted = [b".", file.as_bytes()].concat();
    let mut parts = (name.iter().enumerate()).filter(|(_, byte)| **byte == b'\\');
    hfs(name, &dotted)
        || ntfs_short(name, file.as_bytes(), short)
        || backslashes && parts.any(|(at, _)| ntfs_short(&name[at + 1..], file.as_bytes(), short))
}

/// NTFS as Git reads it for a file of `.<file>`: dots and spaces at the end
/// and a stream after a colon dropped, the short name of the first six
/// letters and `~1` to `~4`, and the short name made of `short` where those
/// are taken
fn ntfs_short(name: &[u8], file: &[u8], short: &[u8; 6]) -> bool {
    let rest = |from: usize| {
        let rest = name.get(from..).unwrap_or_default();
        let end = rest.iter().position(|byte| *byte == b':');
        rest[..end.unwrap_or(rest.len())]
            .iter()
            .all(|byte| matches!(byte, b'.' | b' '))
    };
    let long = name.len() > file.len() && name[0] == b'.';
    if long && name[1..=file.len()].eq_ignore_ascii_case(file) {
        return rest(file.len() + 1);
    }
    if name.len() >= 8
        && name[..6].eq_ignore_ascii_case(&file[..6])
        && name[6] == b'~'
        && (b'1'..=b'4').contains(&name[7])
    {
        return rest(8);
    }
    let mut tilde = false;
    let mut at = 0;
    while at < 8 {
        let Some(&byte) = name.get(at) else {
            return false;
        };
        if tilde {
            if !byte.is_ascii_digit() {
                return false;
            }
        } else if byte == b'~' {
            at += 1;
            if !name
                .get(at)
                .is_some_and(|digit| (b'1'..=b'9').contains(digit))
            {
                return false;
            }
            tilde = true;
        } else if at >= 6 || byte.to_ascii_lowercase() != short[at] {
            return false;
        }
        at += 1;
    }
    rest(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_git_fsck_refuses_and_those_of_alt_are_found_and_others_are_not() {
        for (word, said) in [
            ("git", "hasDotgit: contains '.git'"),
            ("alt", "hasDotalt: contains '.alt'"),
        ] {
            let upper = word.to_ascii_uppercase();
            let (a, rest) = word.split_at(1);
            let unsafe_names = [
                format!(".{word}"),
                format!(".{upper}"),
                format!(".{a}{}. . ", rest.to_ascii_uppercase()),
                format!("{word}~1"),
                format!("{upper}~1 "),
                format!(".{word}::$INDEX_ALLOCATION"),
                format!(".{a}\u{200c}{rest}"),
                format!("\u{feff}.{upper}"),
                format!("a\\.{word}"),
                format!("a\\{word}~1"),
                format!("x\\.{upper}."),
                format!("a\\.{word}\\b"),
                format!("a\\b\\.{word}:x"),
            ];
            for name in unsafe_names {
                assert_eq!(unsafe_name(name.as_bytes()), Some(said), "{name:?}");
            }
            let taken = [
                format!(".{word}ignore"),
                format!(".{word}-x"),
                format!("{word}~2"),
                word.to_owned(),
                format!(".{a}"),
                format!("x.{word}"),
                format!(".{a}\u{200b}{rest}"),
                format!("a\\.{word}x"),
                format!("a\\x.{word}"),
                format!("a\\.{a}\u{200c}{rest}"),
            ];
            for name in taken {
                assert_eq!(unsafe_name(name.as_bytes()), None, "{name:?}");
            }
        }
        assert_eq!(unsafe_name(b""), Some("emptyName: contains empty pathname"));
        assert_eq!(unsafe_name(b"."), Some("hasDot: contains '.'"));
        assert_eq!(unsafe_name(b".."), Some("hasDotdot: contains '..'"));
        let full = Some("fullPathname: contains full pathnames");
        assert_eq!(unsafe_name(b"a/b"), full);
    }

    #[test]
    fn dot_files_are_found_as_git_fsck_finds_them() {
        let gitmodules = |name: &[u8]| dot_file(name, "gitmodules", b"gi7eba", true);
        let found: [&[u8]; 11] = [
            b".gitmodules",
            b".GITMODULES",
            b".gitmodules. .",
            b".gitmodules:x",
            b"gitmod~1",
            b"GITMOD~4 ",
            b"gi7eba~1",
            b".git\xe2\x80\x8cmodules",
            b"a\\.gitmodules",
            b"a\\gitmod~2",
            b".gitmodules\xff",
        ];
        for name in found {
            assert!(gitmodules(name), "{}", name.escape_ascii());
        }
        let not: [&[u8]; 10] = [
            b"gi7eba~12",
            b".gitmodulesx",
            b"gitmod~5",
            b"gi7eba~a",
            b"gi7ebb~1",
            b"gitmodules",
            b"a\\.gitmodules\\b",
            b".gitmodule",
            b"gi7eba~",
            b"gi7eb~1",
        ];
        for name in not {
            assert!(!gitmodules(name), "{}", name.escape_ascii());
        }
        // what follows a backslash is read for `.gitmodules` alone
        assert!(!dot_file(
            b"a\\.gitattributes",
            "gitattributes",
            b"gi7d29",
            false
        ));
        assert!(dot_file(b"gi7d29~1", "gitattributes", b"gi7d29", false));
        // a name whose bytes stop being UTF-8 ends there
        assert_eq!(unsafe_name(b".git\xff"), Some("hasDotgit: contains '.git'"));
        assert_eq!(unsafe_name(b".gi\xfft"), None);
    }
}
