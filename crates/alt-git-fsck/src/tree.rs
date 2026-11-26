//! What `git fsck` says of the entries of one tree.

use crate::{SYMLINK, TREE};
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
pub struct Fault<O> {
    pub name: Vec<u8>,
    pub why: &'static str,
    kind: Kind<O>,
    /// the mode of the entry
    mode: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind<O> {
    /// the name alone, of an entry naming the object
    Name(O),
    /// the name twice in the tree
    Twice,
    /// the name as a symbolic link to the object
    Symlink(O),
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
    pub faults: Vec<Fault<O>>,
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
    let mut fault = |name: &[u8], why, kind, mode| {
        let name = name.to_vec();
        flagged.faults.push(Fault {
            name,
            why,
            kind,
            mode,
        });
    };
    for entry in entries {
        let name = entry.name.as_ref();
        if let Some(why) = unsafe_name(name) {
            fault(name, why, Kind::Name(entry.oid), entry.mode);
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
                    Kind::Symlink(entry.oid),
                    entry.mode,
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
            fault(two[0], why, Kind::Twice, 0);
            break;
        }
    }
    let nothing = flagged.faults.is_empty() && flagged.files.is_empty();
    (!nothing).then_some(flagged)
}

impl<O: PartialEq> Fault<O> {
    /// Whether one of the trees `before` at the same place already had what
    /// the tree is refused for: the name as a directory, the name as the same
    /// entry, by mode and id, where it is no directory, the name twice. A
    /// directory where a file of the name was is brought anew, and so is a
    /// file where a directory was: what a directory holds is not checked
    /// against the file before it. A file, symbolic link or submodule kept by
    /// a name that is refused is kept as it was: other bytes, another mode or
    /// another target under the name are brought anew.
    pub fn had<N: AsRef<[u8]>>(&self, before: &[Vec<Entry<N, O>>]) -> bool {
        let directory = self.mode & 0o170000 == TREE;
        before.iter().any(|tree| {
            let mut same = tree.iter().filter(|entry| entry.name.as_ref() == self.name);
            match &self.kind {
                Kind::Name(_) if directory => same.any(|entry| entry.mode & 0o170000 == TREE),
                Kind::Name(oid) | Kind::Symlink(oid) => {
                    same.any(|entry| entry.mode == self.mode && entry.oid == *oid)
                }
                Kind::Twice => same.count() >= 2,
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
        // a file where a directory was, a symbolic link where a file was, and
        // a name once where it is twice now
        assert_eq!(had, [false, false, false]);
        let file_before = vec![vec![entry(b".alt", 0o100644, 1)]];
        assert!(found.faults[0].had(&file_before));
        // a file kept by the name is kept as it was: other bytes or another
        // mode are new
        assert!(!found.faults[0].had(&[vec![entry(b".alt", 0o100644, 9)]]));
        assert!(!found.faults[0].had(&[vec![entry(b".alt", 0o100755, 1)]]));
        let directory = flagged(&[entry(b".alt", 0o040000, 9)]).unwrap();
        assert!(!directory.faults[0].had(&file_before));
        assert!(directory.faults[0].had(&before));
        let twice = vec![
            before[0].clone(),
            vec![entry(b"x", 0o100644, 3), entry(b"x", 0o100644, 3)],
        ];
        assert!(found.faults[2].had(&twice));
        let link = vec![vec![entry(b".gitmodules", 0o120000, 2)]];
        assert!(found.faults[1].had(&link));
        // a symbolic link to another target is new
        assert!(!found.faults[1].had(&[vec![entry(b".gitmodules", 0o120000, 7)]]));
        // a symbolic link or a submodule kept by a name that is refused is
        // kept as it was
        let kept = |mode, oid| flagged(&[entry(b"GIT~1", mode, oid)]).unwrap().faults;
        for mode in [0o120000, 0o160000] {
            let before = vec![vec![entry(b"GIT~1", mode, 3)]];
            assert!(kept(mode, 3)[0].had(&before));
            assert!(!kept(mode, 4)[0].had(&before));
        }
        assert!(!kept(0o100644, 3)[0].had(&[vec![entry(b"GIT~1", 0o120000, 3)]]));

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
