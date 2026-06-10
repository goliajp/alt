use core::fmt;

use bstr::{BString, ByteSlice};

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

impl Tree {
    /// Parses tree data; `algo` determines the raw oid width.
    pub fn parse(data: &[u8], algo: HashAlgo) -> Result<Self, ObjectParseError> {
        let raw_len = algo.raw_len();
        let mut entries = Vec::new();
        let mut rest = data;
        while !rest.is_empty() {
            let sp = rest
                .find_byte(b' ')
                .ok_or(ObjectParseError::Tree("entry without space after mode"))?;
            let mode = EntryMode::from_bytes(&rest[..sp])?;
            rest = &rest[sp + 1..];

            let nul = rest
                .find_byte(0)
                .ok_or(ObjectParseError::Tree("entry name not NUL-terminated"))?;
            let name: BString = rest[..nul].into();
            rest = &rest[nul + 1..];

            if rest.len() < raw_len {
                return Err(ObjectParseError::Tree("truncated entry oid"));
            }
            let oid = ObjectId::from_bytes(algo, &rest[..raw_len]).unwrap();
            rest = &rest[raw_len..];

            entries.push(TreeEntry { mode, name, oid });
        }
        Ok(Self { entries })
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
    bytes: [u8; 6],
    len: u8,
}

impl EntryMode {
    /// Accepts 1–6 octal digits, as git's tree parser does.
    pub fn from_bytes(mode: &[u8]) -> Result<Self, ObjectParseError> {
        if mode.is_empty() || mode.len() > 6 {
            return Err(ObjectParseError::Tree("entry mode must be 1-6 octal digits"));
        }
        if !mode.iter().all(|b| (b'0'..=b'7').contains(b)) {
            return Err(ObjectParseError::Tree("entry mode has non-octal digit"));
        }
        let mut bytes = [0u8; 6];
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
    }

    #[test]
    fn rejects_malformed_trees() {
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"x");
        for data in [
            b"100644".to_vec(),                       // no space
            b"100648 a\0".to_vec(),                   // non-octal mode
            b"100644 noterm".to_vec(),                // no NUL
            entry_bytes("100644", b"a", &oid)[..20].to_vec(), // truncated oid
        ] {
            assert!(Tree::parse(&data, HashAlgo::Sha1).is_err(), "{data:?}");
        }
    }
}
