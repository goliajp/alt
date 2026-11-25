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

/// `.` and `word` as Git, NTFS or HFS+ reads the name
fn read_as(name: &[u8], word: &[u8; 3]) -> bool {
    let dotted = [&b"."[..], word].concat();
    name.eq_ignore_ascii_case(&dotted) || ntfs(name, word) || hfs(name, &dotted)
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

/// HFS+ leaves out code points that take no space, and folds case
fn hfs(name: &[u8], dotted: &[u8]) -> bool {
    let Ok(name) = std::str::from_utf8(name) else {
        return false;
    };
    let ignored = |c: &char| matches!(*c as u32, 0x200c..=0x200f | 0x202a..=0x202e | 0x206a..=0x206f | 0xfeff);
    let mut left = name.chars().filter(|c| !ignored(c));
    let mut needle = dotted.iter().map(|byte| char::from(*byte));
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
}
