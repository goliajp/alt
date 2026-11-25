//! The names a tree may not give its entries, as `git fsck` tells them: what a
//! checkout would write outside the directory of the tree, into the directory
//! of Git or of alt on some file system, or twice. A tree that only is not written
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
        if let Some(wrong) = alt_git_codec::unsafe_name(name) {
            return Some(wrong);
        }
        names.push(name);
        rest = rest.get(nul + 21..)?;
    }
    names.sort_unstable();
    let twice = names.windows(2).any(|two| two[0] == two[1]);
    twice.then_some("duplicateEntries: contains duplicate file entries")
}

/// What a push is refused for in tree `data`: [`found`], or a mode of more
/// digits than a reader of trees reads.
pub(crate) fn refused(data: &[u8]) -> Option<&'static str> {
    let mut rest = data;
    while let Some(space) = rest.iter().position(|byte| *byte == b' ') {
        if space > 16 {
            return Some("badMode: contains bad file modes");
        }
        let nul = rest.iter().position(|byte| *byte == 0)?;
        rest = rest.get(nul + 21..)?;
    }
    found(data)
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
    fn names_git_fsck_refuses_and_entries_twice_are_found_and_others_are_not() {
        let said = |name: &[u8]| found(&tree(&[b"a", name]));
        assert_eq!(said(b".GIT"), Some("hasDotgit: contains '.git'"));
        assert_eq!(said(b"alt~1"), Some("hasDotalt: contains '.alt'"));
        assert_eq!(said(b".."), Some("hasDotdot: contains '..'"));
        assert_eq!(said(b".gitignore"), None);
        let twice = Some("duplicateEntries: contains duplicate file entries");
        assert_eq!(found(&tree(&[b"b", b"a", b"b"])), twice);
        // out of order, and a mode with a zero before it
        assert_eq!(found(&tree(&[b"b", b"a"])), None);
        let mut padded = b"040000 d\0".to_vec();
        padded.extend_from_slice(&[7; 20]);
        assert_eq!(found(&padded), None);
        // what a push is refused for also holds a mode too long to be read
        assert_eq!(refused(&padded), None);
        assert_eq!(
            refused(&tree(&[b"a", b".git"])),
            Some("hasDotgit: contains '.git'")
        );
        let mut long = b"00000000000100644 f\0".to_vec();
        long.extend_from_slice(&[7; 20]);
        assert_eq!(
            refused(&[&padded[..], &long].concat()),
            Some("badMode: contains bad file modes")
        );
        assert_eq!(refused(&long[1..]), None);
    }
}
