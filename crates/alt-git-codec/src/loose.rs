use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::{ObjectId, ObjectKind};

/// The longest legal loose header: `"commit"` + space + decimal size + NUL.
/// 32 bytes leaves room for any 64-bit size.
const MAX_HEADER: usize = 32;

/// A decoded object: its kind plus the raw payload bytes (no header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawObject {
    pub kind: ObjectKind,
    pub data: Vec<u8>,
}

/// Read access to the loose objects under a `.git/objects` directory.
#[derive(Debug, Clone)]
pub struct LooseStore {
    objects_dir: PathBuf,
}

impl LooseStore {
    pub fn new(objects_dir: impl Into<PathBuf>) -> Self {
        Self {
            objects_dir: objects_dir.into(),
        }
    }

    pub fn objects_dir(&self) -> &Path {
        &self.objects_dir
    }

    /// The fan-out path of `oid`: `objects/aa/bbbb…`.
    pub fn path_of(&self, oid: &ObjectId) -> PathBuf {
        let hex = oid.to_string();
        self.objects_dir.join(&hex[..2]).join(&hex[2..])
    }

    pub fn contains(&self, oid: &ObjectId) -> bool {
        self.path_of(oid).is_file()
    }

    /// Reads and inflates the loose object `oid`.
    pub fn read(&self, oid: &ObjectId) -> Result<RawObject, LooseError> {
        let path = self.path_of(oid);
        let file = File::open(&path).map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => LooseError::NotFound(*oid),
            _ => LooseError::Io { oid: *oid, source },
        })?;
        let mut decoder = alt_inflate::Reader::new(alt_inflate::Format::Zlib, file);

        let (kind, size) = parse_header(&mut decoder, oid)?;
        let mut data = Vec::with_capacity(size);
        decoder
            .read_to_end(&mut data)
            .map_err(|source| LooseError::Io { oid: *oid, source })?;
        if data.len() != size {
            return Err(LooseError::Corrupt {
                oid: *oid,
                reason: "payload length does not match header size",
            });
        }
        Ok(RawObject { kind, data })
    }
}

/// Parses `"<kind> <size>\0"` from the head of the inflated stream.
fn parse_header(reader: &mut impl Read, oid: &ObjectId) -> Result<(ObjectKind, usize), LooseError> {
    let mut header = [0u8; MAX_HEADER];
    let mut len = 0;
    loop {
        let mut byte = [0u8; 1];
        reader
            .read_exact(&mut byte)
            .map_err(|source| LooseError::Io { oid: *oid, source })?;
        if byte[0] == 0 {
            break;
        }
        if len == MAX_HEADER {
            return Err(LooseError::Corrupt {
                oid: *oid,
                reason: "header not NUL-terminated within 32 bytes",
            });
        }
        header[len] = byte[0];
        len += 1;
    }
    let header = &header[..len];

    let space = header
        .iter()
        .position(|&b| b == b' ')
        .ok_or(LooseError::Corrupt {
            oid: *oid,
            reason: "header has no space separator",
        })?;
    let kind = ObjectKind::from_bytes(&header[..space]).map_err(|_| LooseError::Corrupt {
        oid: *oid,
        reason: "unknown object kind in header",
    })?;
    let size = core::str::from_utf8(&header[space + 1..])
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or(LooseError::Corrupt {
            oid: *oid,
            reason: "invalid decimal size in header",
        })?;
    Ok((kind, size))
}

#[derive(Debug)]
pub enum LooseError {
    NotFound(ObjectId),
    Io { oid: ObjectId, source: io::Error },
    Corrupt { oid: ObjectId, reason: &'static str },
}

impl std::fmt::Display for LooseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(oid) => write!(f, "object {oid} not found in loose store"),
            Self::Io { oid, .. } => write!(f, "io error on loose object {oid}"),
            Self::Corrupt { oid, reason } => write!(f, "corrupt loose object {oid}: {reason}"),
        }
    }
}

impl std::error::Error for LooseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::NotFound(_) | Self::Corrupt { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::HashAlgo;

    /// Writes `raw` zlib-compressed to the loose path of `oid` and returns the store.
    fn store_with(dir: &Path, oid: &ObjectId, raw: &[u8]) -> LooseStore {
        let store = LooseStore::new(dir);
        let path = store.path_of(oid);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let encoded = alt_deflate::compress(raw, 6, alt_deflate::Format::Zlib);
        fs::write(path, encoded).unwrap();
        store
    }

    #[test]
    fn reads_back_what_it_stores() {
        let dir = alt_os::temp::tempdir().unwrap();
        let payload = b"hello world\n";
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, payload);
        let store = store_with(dir.path(), &oid, b"blob 12\0hello world\n");

        assert!(store.contains(&oid));
        let obj = store.read(&oid).unwrap();
        assert_eq!(obj.kind, ObjectKind::Blob);
        assert_eq!(obj.data, payload);
        assert_eq!(
            ObjectId::hash_object(HashAlgo::Sha1, obj.kind, &obj.data),
            oid
        );
    }

    #[test]
    fn missing_object_is_not_found() {
        let dir = alt_os::temp::tempdir().unwrap();
        let store = LooseStore::new(dir.path());
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"");
        assert!(!store.contains(&oid));
        assert!(matches!(store.read(&oid), Err(LooseError::NotFound(o)) if o == oid));
    }

    #[test]
    fn corrupt_headers_are_rejected() {
        let dir = alt_os::temp::tempdir().unwrap();
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"x");
        for raw in [
            &b"blob12\0x"[..],                             // no space
            &b"sock 1\0x"[..],                             // unknown kind
            &b"blob nan\0x"[..],                           // bad size
            &b"blob 2\0x"[..],                             // size mismatch
            &b"blob 11111111111111111111111111111\0x"[..], // header too long
        ] {
            let store = store_with(dir.path(), &oid, raw);
            assert!(
                matches!(store.read(&oid), Err(LooseError::Corrupt { .. })),
                "raw {raw:?} should be corrupt"
            );
        }
    }
}
