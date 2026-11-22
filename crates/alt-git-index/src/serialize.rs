//! Writing an index out.

use alt_git_codec::HashAlgo;

use crate::{Index, IndexEntry};

impl Index {
    /// Serializes the index. Entries are emitted sorted by (path, stage) as
    /// git requires. Versions 2 and 3 are written faithfully; version 4's
    /// path-prefix compression is a size optimization, so a v4 index is
    /// written as v2 (full paths) — lossless, just larger. The trailer hash
    /// is computed over the content.
    pub fn serialize(&self, algo: HashAlgo) -> Vec<u8> {
        Self::sealed(self.content(algo), algo)
    }

    /// `content` ([`Self::content`]) with the trailer hash an index file
    /// ends in.
    pub fn sealed(mut content: Vec<u8>, algo: HashAlgo) -> Vec<u8> {
        let trailer: Vec<u8> = match algo {
            HashAlgo::Sha1 => alt_hash::sha1(&content).to_vec(),
            HashAlgo::Sha256 => alt_hash::sha256(&content).to_vec(),
        };
        content.extend_from_slice(&trailer);
        content
    }

    /// What [`Self::serialize`] writes before the trailer hash.
    pub fn content(&self, algo: HashAlgo) -> Vec<u8> {
        let raw = algo.raw_len();
        // the minimal faithful version: v3 only if an entry needs extended
        // flags, else v2 (v4 downgrades here)
        let has_extended = self.entries.iter().any(|e| e.extended_flags.is_some());
        let version: u32 = if has_extended { 3 } else { 2 };

        let mut sorted: Vec<&IndexEntry> = self.entries.iter().collect();
        sorted.sort_by(|a, b| a.path.cmp(&b.path).then(a.stage().cmp(&b.stage())));

        let mut out = Vec::with_capacity(12 + sorted.len() * (40 + raw + 8));
        out.extend_from_slice(b"DIRC");
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&(sorted.len() as u32).to_be_bytes());

        for e in &sorted {
            let start = out.len();
            for v in [
                e.ctime.0, e.ctime.1, e.mtime.0, e.mtime.1, e.dev, e.ino, e.mode, e.uid, e.gid,
                e.size,
            ] {
                out.extend_from_slice(&v.to_be_bytes());
            }
            out.extend_from_slice(e.oid.as_bytes());
            // rebuild flags: keep the high control bits, recompute the name
            // length, and set the extended bit exactly when we emit one
            let name_len = e.path.len().min(0x0FFF) as u16;
            let mut flags = (e.flags & 0xF000) | name_len;
            flags = match e.extended_flags {
                Some(_) => flags | 0x4000,
                None => flags & !0x4000,
            };
            out.extend_from_slice(&flags.to_be_bytes());
            if let Some(ext) = e.extended_flags {
                out.extend_from_slice(&ext.to_be_bytes());
            }
            out.extend_from_slice(&e.path);
            // pad with 1–8 NULs to an 8-byte multiple of the whole entry
            let pad = 8 - ((out.len() - start) % 8);
            out.resize(out.len() + pad, 0);
        }

        for ext in &self.extensions {
            out.extend_from_slice(&ext.signature);
            out.extend_from_slice(&(ext.data.len() as u32).to_be_bytes());
            out.extend_from_slice(&ext.data);
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::{ObjectId, ObjectKind};

    #[test]
    fn the_content_is_what_is_serialized_before_the_trailer() {
        let entry = |algo: HashAlgo, path: &str, mtime: u32| IndexEntry {
            ctime: (1, 2),
            mtime: (mtime, 4),
            dev: 5,
            ino: 6,
            mode: 0o100644,
            uid: 7,
            gid: 8,
            size: 9,
            oid: ObjectId::hash_object(algo, ObjectKind::Blob, path.as_bytes()),
            flags: 0,
            extended_flags: None,
            path: path.into(),
        };
        for algo in [HashAlgo::Sha1, HashAlgo::Sha256] {
            let index = Index {
                version: 2,
                entries: vec![entry(algo, "b", 3), entry(algo, "a/c", 0)],
                extensions: Vec::new(),
            };
            let whole = index.serialize(algo);
            let content = index.content(algo);
            assert_eq!(whole[..whole.len() - algo.raw_len()], content[..]);
            assert_eq!(Index::sealed(content, algo), whole);
            assert_eq!(Index::parse(&whole, algo).unwrap().serialize(algo), whole);
        }
    }
}
