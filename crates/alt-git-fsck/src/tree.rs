//! What `git fsck` says of the entries of one tree.

use crate::SYMLINK;
use alt_git_codec::{dot_file, unsafe_name};

/// an entry of a tree: its name, its mode as a number, and the id it names
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry<N, O> {
    pub name: N,
    pub mode: u32,
    pub oid: O,
}

/// An entry `git fsck` refuses a tree for, and in what words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub name: Vec<u8>,
    pub why: &'static str,
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// the name alone
    Name,
    /// the name twice in the tree
    Twice,
    /// the name as a symbolic link
    Symlink,
}

/// The files whose blobs `git fsck` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Special {
    Gitmodules,
    Gitattributes,
}

/// an entry whose object is read as a [`Special`] file
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File<O> {
    pub name: Vec<u8>,
    pub oid: O,
    pub special: Special,
}

/// What of a tree `git fsck` looks at again: the entries it is refused for,
/// and the files whose blobs are read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flagged<O> {
    pub faults: Vec<Fault>,
    pub files: Vec<File<O>>,
}

fn symlink(mode: u32) -> bool {
    mode & 0o170000 == SYMLINK
}

/// What `git fsck` looks at again of a tree of `entries`, where it does at
/// anything: none where it does not.
pub fn flagged<N: AsRef<[u8]>, O: Copy>(entries: &[Entry<N, O>]) -> Option<Flagged<O>> {
    let mut flagged = Flagged {
        faults: Vec::new(),
        files: Vec::new(),
    };
    let mut fault = |name: &[u8], why, kind| {
        let name = name.to_vec();
        flagged.faults.push(Fault { name, why, kind });
    };
    for entry in entries {
        let name = entry.name.as_ref();
        if let Some(why) = unsafe_name(name) {
            fault(name, why, Kind::Name);
        }
        let mut file = |special| {
            let (name, oid) = (name.to_vec(), entry.oid);
            flagged.files.push(File { name, oid, special });
        };
        if dot_file(name, "gitmodules", b"gi7eba", true) {
            match symlink(entry.mode) {
                true => fault(
                    name,
                    "gitmodulesSymlink: .gitmodules is a symbolic link",
                    Kind::Symlink,
                ),
                false => file(Special::Gitmodules),
            }
        }
        if dot_file(name, "gitattributes", b"gi7d29", false) && !symlink(entry.mode) {
            file(Special::Gitattributes);
        }
    }
    let mut names: Vec<&[u8]> = entries.iter().map(|entry| entry.name.as_ref()).collect();
    names.sort_unstable();
    for two in names.windows(2) {
        if two[0] == two[1] {
            let why = "duplicateEntries: contains duplicate file entries";
            fault(two[0], why, Kind::Twice);
            break;
        }
    }
    let nothing = flagged.faults.is_empty() && flagged.files.is_empty();
    (!nothing).then_some(flagged)
}

impl Fault {
    /// Whether one of the trees `before` at the same place already had what
    /// the tree is refused for: the name, the name twice, the name as a
    /// symbolic link.
    pub fn had<N: AsRef<[u8]>, O>(&self, before: &[Vec<Entry<N, O>>]) -> bool {
        before.iter().any(|tree| {
            let mut same = tree.iter().filter(|entry| entry.name.as_ref() == self.name);
            match self.kind {
                Kind::Name => same.next().is_some(),
                Kind::Twice => same.count() >= 2,
                Kind::Symlink => same.any(|entry| symlink(entry.mode)),
            }
        })
    }
}

impl<O: PartialEq> File<O> {
    /// Whether one of the trees `before` at the same place named the same
    /// object by the same name: its blob is then none the push brings there.
    pub fn had<N: AsRef<[u8]>>(&self, before: &[Vec<Entry<N, O>>]) -> bool {
        before.iter().flatten().any(|entry| {
            entry.name.as_ref() == self.name && entry.oid == self.oid && !symlink(entry.mode)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &[u8], mode: u32, oid: u8) -> Entry<Vec<u8>, u8> {
        Entry {
            name: name.to_vec(),
            mode,
            oid,
        }
    }

    #[test]
    fn faults_and_files_are_found_and_what_the_tree_before_had_is_known() {
        let plain = [entry(b"a", 0o100644, 1), entry(b"b", 0o040000, 2)];
        assert_eq!(flagged(&plain), None);

        let tree = [
            entry(b".alt", 0o100644, 1),
            entry(b".gitmodules", 0o120000, 2),
            entry(b"x", 0o100644, 3),
            entry(b"x", 0o040000, 4),
            entry(b"gi7d29~1", 0o100644, 5),
        ];
        let found = flagged(&tree).unwrap();
        let why: Vec<&str> = found.faults.iter().map(|fault| fault.why).collect();
        assert_eq!(
            why,
            [
                "hasDotalt: contains '.alt'",
                "gitmodulesSymlink: .gitmodules is a symbolic link",
                "duplicateEntries: contains duplicate file entries",
            ]
        );
        let files: Vec<(Special, u8)> = (found.files.iter())
            .map(|file| (file.special, file.oid))
            .collect();
        assert_eq!(files, [(Special::Gitattributes, 5)]);

        // what a tree before at the same place had
        let none: [Vec<Entry<Vec<u8>, u8>>; 0] = [];
        assert!(found.faults.iter().all(|fault| !fault.had(&none)));
        let before = vec![vec![
            entry(b".alt", 0o040000, 9),
            entry(b".gitmodules", 0o100644, 2),
            entry(b"x", 0o100644, 3),
        ]];
        let had: Vec<bool> = found
            .faults
            .iter()
            .map(|fault| fault.had(&before))
            .collect();
        // a symbolic link where a file was, and a name once where it is twice now
        assert_eq!(had, [true, false, false]);
        let twice = vec![
            before[0].clone(),
            vec![entry(b"x", 0o100644, 3), entry(b"x", 0o100644, 3)],
        ];
        assert!(found.faults[2].had(&twice));
        let link = vec![vec![entry(b".gitmodules", 0o120000, 7)]];
        assert!(found.faults[1].had(&link));

        let file = &found.files[0];
        assert!(!file.had(&before));
        assert!(file.had(&[vec![entry(b"gi7d29~1", 0o100644, 5)]]));
        assert!(!file.had(&[vec![entry(b"gi7d29~1", 0o100644, 6)]]));

        // `.gitmodules` that is no symbolic link is read
        let modules = flagged(&[entry(b"GITMOD~1", 0o100644, 8)]).unwrap();
        assert!(modules.faults.is_empty());
        assert_eq!(modules.files[0].special, Special::Gitmodules);
    }
}
