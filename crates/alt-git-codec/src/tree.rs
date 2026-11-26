use core::fmt;

use alt_bytes::{BString, ByteSlice};

use crate::{HashAlgo, ObjectId, ObjectKind, ObjectParseError};

/// A parsed tree object: a sequence of `"<mode> <name>\0<raw oid>"` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: EntryMode,
    /// Entry name: arbitrary bytes, not necessarily UTF-8.
    pub name: BString,
    pub oid: ObjectId,
}

impl TreeEntry {
    /// The order entries have inside a tree: by name, a directory comparing
    /// as if its name ended in `/`. Two trees walked in step have to be
    /// compared this way: by plain name `lib` sorts before `lib-old.c`,
    /// in a tree after it. A file and a directory of one name are unequal.
    pub fn tree_order(&self, other: &TreeEntry) -> core::cmp::Ordering {
        let key = |e: &TreeEntry| {
            let slash = e.mode.is_tree().then_some(b'/');
            e.name.iter().copied().chain(slash).collect::<Vec<u8>>()
        };
        key(self).cmp(&key(other))
    }
}

impl Tree {
    /// Parses tree data; `algo` determines the raw oid width.
    pub fn parse(data: &[u8], algo: HashAlgo) -> Result<Self, ObjectParseError> {
        let mut entries = Vec::new();
        for entry in Self::raw_entries(data, algo) {
            let (mode, name, oid) = entry?;
            entries.push(TreeEntry {
                mode,
                name: name.into(),
                oid: ObjectId::from_bytes(algo, oid).unwrap(),
            });
        }
        Ok(Self { entries })
    }

    /// The entries of tree `data` where they are stored: mode, name and raw
    /// object id. The first damaged entry ends the iteration with its error.
    pub fn raw_entries(data: &[u8], algo: HashAlgo) -> RawEntries<'_> {
        RawEntries {
            rest: data,
            raw_len: algo.raw_len(),
        }
    }

    /// The mode and id of the entry called `name` in tree `data`, found
    /// without building the tree: for a caller that follows one path
    /// through many trees. Entries are checked up to the one found.
    pub fn find(
        data: &[u8],
        algo: HashAlgo,
        name: &[u8],
    ) -> Result<Option<(u32, ObjectId)>, ObjectParseError> {
        for entry in Self::raw_entries(data, algo) {
            let (mode, found, oid) = entry?;
            if found == name {
                return Ok(ObjectId::from_bytes(algo, oid)
                    .ok()
                    .map(|oid| (mode.value(), oid)));
            }
        }
        Ok(None)
    }

    pub fn serialize_into(&self, out: &mut Vec<u8>) {
        for entry in &self.entries {
            out.extend_from_slice(entry.mode.as_bytes());
            out.push(b' ');
            out.extend_from_slice(&entry.name);
            out.push(0);
            out.extend_from_slice(entry.oid.as_bytes());
        }
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.serialize_into(&mut out);
        out
    }
}

/// A tree entry mode, kept as the original octal ASCII bytes.
///
/// Stored raw rather than as a number because real-world history contains
/// non-canonical spellings (e.g. `100664`, zero-padded modes) and re-encoding
/// them canonically would change the tree's hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryMode {
    bytes: [u8; MODE_DIGITS],
    len: u8,
}

/// The most digits a mode is written with: six, and zeros before them, which
/// Git reads as the same mode.
pub const MODE_DIGITS: usize = 16;

impl EntryMode {
    /// Accepts 1 to [`MODE_DIGITS`] octal digits whose value has six at the
    /// most, as Git's tree parser reads them.
    pub fn from_bytes(mode: &[u8]) -> Result<Self, ObjectParseError> {
        let digits = mode.iter().skip_while(|digit| **digit == b'0').count();
        if mode.is_empty() || mode.len() > MODE_DIGITS || digits > 6 {
            return Err(ObjectParseError::Tree(
                "entry mode must be 1-6 octal digits, after zeros",
            ));
        }
        if !mode.iter().all(|b| (b'0'..=b'7').contains(b)) {
            return Err(ObjectParseError::Tree("entry mode has non-octal digit"));
        }
        let mut bytes = [0u8; MODE_DIGITS];
        bytes[..mode.len()].copy_from_slice(mode);
        Ok(Self {
            bytes,
            len: mode.len() as u8,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap()
    }

    /// The numeric mode value (parsed octal).
    pub fn value(&self) -> u32 {
        self.as_bytes()
            .iter()
            .fold(0, |acc, b| (acc << 3) | u32::from(b - b'0'))
    }

    pub fn is_tree(&self) -> bool {
        self.value() & 0o170000 == 0o040000
    }

    pub fn is_gitlink(&self) -> bool {
        self.value() & 0o170000 == 0o160000
    }

    pub fn is_symlink(&self) -> bool {
        self.value() & 0o170000 == 0o120000
    }

    /// The object kind this entry points at.
    pub fn object_kind(&self) -> ObjectKind {
        if self.is_tree() {
            ObjectKind::Tree
        } else if self.is_gitlink() {
            ObjectKind::Commit
        } else {
            ObjectKind::Blob
        }
    }
}

impl fmt::Display for EntryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for EntryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntryMode({})", self.as_str())
    }
}

/// See [`Tree::raw_entries`].
pub struct RawEntries<'a> {
    rest: &'a [u8],
    raw_len: usize,
}

impl<'a> RawEntries<'a> {
    fn entry(&mut self) -> Result<(EntryMode, &'a [u8], &'a [u8]), ObjectParseError> {
        let rest = self.rest;
        let sp = rest
            .find_byte(b' ')
            .ok_or(ObjectParseError::Tree("entry without space after mode"))?;
        let mode = EntryMode::from_bytes(&rest[..sp])?;
        let rest = &rest[sp + 1..];
        let nul = rest
            .find_byte(0)
            .ok_or(ObjectParseError::Tree("entry name not NUL-terminated"))?;
        let (name, rest) = (&rest[..nul], &rest[nul + 1..]);
        if rest.len() < self.raw_len {
            return Err(ObjectParseError::Tree("truncated entry oid"));
        }
        let (oid, rest) = rest.split_at(self.raw_len);
        self.rest = rest;
        Ok((mode, name, oid))
    }
}

impl<'a> Iterator for RawEntries<'a> {
    type Item = Result<(EntryMode, &'a [u8], &'a [u8]), ObjectParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let entry = self.entry();
        if entry.is_err() {
            self.rest = &[];
        }
        Some(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_bytes(mode: &str, name: &[u8], oid: &ObjectId) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(name);
        out.push(0);
        out.extend_from_slice(oid.as_bytes());
        out
    }

    #[test]
    fn parses_and_round_trips_including_noncanonical_modes() {
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"x");
        let mut data = Vec::new();
        data.extend(entry_bytes("100644", b"plain.txt", &oid));
        data.extend(entry_bytes("100664", b"group-writable", &oid)); // non-canonical
        data.extend(entry_bytes("40000", b"dir", &oid));
        data.extend(entry_bytes("160000", b"submodule", &oid));
        data.extend(entry_bytes("120000", b"\xff\xfe not utf8", &oid)); // raw-bytes name

        let tree = Tree::parse(&data, HashAlgo::Sha1).unwrap();
        assert_eq!(tree.entries.len(), 5);
        assert!(tree.entries[2].mode.is_tree());
        assert!(tree.entries[3].mode.is_gitlink());
        assert!(tree.entries[4].mode.is_symlink());
        assert_eq!(tree.entries[1].mode.as_str(), "100664");
        assert_eq!(tree.serialize(), data);

        // zeros before a mode, as many as sixteen digits: what Git reads them as
        let mut padded = entry_bytes("040000", b"dir", &oid);
        padded.extend(entry_bytes("0000000000100644", b"file", &oid));
        let tree = Tree::parse(&padded, HashAlgo::Sha1).unwrap();
        assert_eq!(tree.entries[0].mode.value(), 0o40000);
        assert_eq!(tree.entries[1].mode.value(), 0o100644);
        assert_eq!(tree.serialize(), padded);
        for mode in ["00000000000100644", "1000644"] {
            let data = entry_bytes(mode, b"file", &oid);
            assert!(Tree::parse(&data, HashAlgo::Sha1).is_err(), "{mode}");
        }
    }

    #[test]
    fn rejects_malformed_trees() {
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"x");
        for data in [
            b"100644".to_vec(),                               // no space
            b"100648 a\0".to_vec(),                           // non-octal mode
            b"100644 noterm".to_vec(),                        // no NUL
            entry_bytes("100644", b"a", &oid)[..20].to_vec(), // truncated oid
        ] {
            assert!(Tree::parse(&data, HashAlgo::Sha1).is_err(), "{data:?}");
        }
    }

    #[test]
    fn finds_an_entry_by_name_without_building_the_tree() {
        let algo = HashAlgo::Sha1;
        let first = ObjectId::hash_object(algo, ObjectKind::Blob, b"first");
        let second = ObjectId::hash_object(algo, ObjectKind::Blob, b"second");
        let mut data = entry_bytes("40000", b"ab", &first);
        data.extend(entry_bytes("100644", b"abc", &second));
        assert_eq!(
            Tree::find(&data, algo, b"ab").unwrap(),
            Some((0o40000, first))
        );
        assert_eq!(
            Tree::find(&data, algo, b"abc").unwrap(),
            Some((0o100644, second))
        );
        assert_eq!(Tree::find(&data, algo, b"a").unwrap(), None);
        assert_eq!(Tree::find(&[], algo, b"a").unwrap(), None);

        // a damaged entry before the one looked for is an error, as in a parse
        let mut damaged = b"100648 x\0".to_vec();
        damaged.extend_from_slice(first.as_bytes());
        damaged.extend(entry_bytes("100644", b"abc", &second));
        assert!(Tree::find(&damaged, algo, b"abc").is_err());
        let cut = &data[..data.len() - 1];
        assert!(Tree::find(cut, algo, b"abc").is_err());
    }
}
