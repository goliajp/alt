//! An index together with the bytes of the file it was parsed from. A
//! command that reads the index, works out another and puts it in place
//! has to know two things of the one it read: that the file still holds
//! it, and the form of it that recovery snapshots are kept in. With the
//! bytes at hand neither takes another pass of parsing and hashing where
//! the file is still what it was and was written in that form, which is
//! how every index change leaves it.

use super::*;

/// The index of a workspace as it was read; `None` for the bytes where
/// there was no index file.
pub(in crate::native) struct AsRead {
    index: Index,
    bytes: Option<Vec<u8>>,
}

impl std::ops::Deref for AsRead {
    type Target = Index;

    fn deref(&self) -> &Index {
        &self.index
    }
}

impl AsRead {
    /// [`canonical_index`] of the index: the bytes it was read from where
    /// they are that form already, whose trailer was checked when they
    /// were parsed.
    pub(super) fn canonical(self, algo: HashAlgo) -> Vec<u8> {
        let content = canonical(self.index).content(algo);
        let whole = content.len() + algo.raw_len();
        match self.bytes {
            Some(bytes) if bytes.len() == whole && bytes.starts_with(&content) => bytes,
            _ => Index::sealed(content, algo),
        }
    }
}

impl NativeRepo<'_> {
    /// The workspace's index, an empty one where there is no index file,
    /// with the bytes it was read from.
    pub(in crate::native) fn index_as_read(&self) -> Res<AsRead> {
        let (index, bytes) = self.read_index()?;
        Ok(AsRead { index, bytes })
    }

    /// Whether the index file holds the index `read` was read as: the
    /// same bytes, or failing that the same index in another form.
    pub(super) fn unchanged_since(&self, read: &AsRead) -> Res<bool> {
        let same = match &read.bytes {
            Some(bytes) => crate::durable::holds(&self.index_path, bytes)? == Some(true),
            None => crate::durable::holds(&self.index_path, &[])?.is_none(),
        };
        if same {
            return Ok(true);
        }
        let algo = self.store.algo()?;
        Ok(canonical_index(self.index()?, algo) == canonical_index(read.index.clone(), algo))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::ObjectKind;

    fn index(stat: u32) -> Index {
        let entry = |path: &str, flags: u16| IndexEntry {
            ctime: (stat, stat),
            mtime: (stat, stat),
            dev: stat,
            ino: stat,
            mode: 0o100644,
            uid: stat,
            gid: stat,
            size: stat,
            oid: ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, path.as_bytes()),
            flags,
            extended_flags: None,
            path: path.into(),
        };
        Index {
            version: 2,
            // a conflict's stages among plain entries, out of order
            entries: vec![
                entry("z", 0),
                entry("m", 2 << 12),
                entry("m", 1 << 12),
                entry("a/b", 0),
            ],
            extensions: Vec::new(),
        }
    }

    fn as_read(bytes: Option<Vec<u8>>) -> AsRead {
        let index = match &bytes {
            Some(bytes) => Index::parse(bytes, HashAlgo::Sha1).unwrap(),
            None => index_file::empty_index(),
        };
        AsRead { index, bytes }
    }

    #[test]
    fn the_canonical_form_is_the_same_whichever_bytes_the_index_was_read_from() {
        let algo = HashAlgo::Sha1;
        let canonical = canonical_index(index(7), algo);
        // written with its stat fields, in the canonical form, and not at all
        assert_eq!(
            as_read(Some(index(7).serialize(algo))).canonical(algo),
            canonical
        );
        assert_eq!(as_read(Some(canonical.clone())).canonical(algo), canonical);
        assert_eq!(
            as_read(None).canonical(algo),
            canonical_index(index_file::empty_index(), algo)
        );
        // read from a file in that form, it is that file's bytes: no other were made
        let read = as_read(Some(canonical));
        let held = read.bytes.as_ref().unwrap().as_ptr();
        let same = read.canonical(algo);
        assert_eq!(same.as_ptr(), held);
    }
}
