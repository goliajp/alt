//! Reusable plain-object zlib streams. They are derived encodings, never an
//! object source: a writer compares the entire decoded stream with its current
//! canonical input before emitting any cached bytes.
use std::sync::Arc;

use alt_git_codec::{ObjectId, ObjectKind};

/// Optional retention bounds while ingesting a pack. Zero disables retention.
/// `bytes` bounds compressed payloads, `objects` bounds entry metadata, and
/// `object_bytes` bounds both the encoded and decoded size of each entry.
/// Reaching any bound skips retention; it does not reject a valid pack.
#[derive(Clone, Copy, Debug, Default)]
pub struct RepresentationLimits {
    pub bytes: usize,
    pub objects: usize,
    pub object_bytes: usize,
}

/// An independently decodable representation of one original object. Only
/// plain entries are retained: no delta-base availability or chain is assumed.
#[derive(Clone, Debug)]
pub struct PlainRepresentation {
    oid: ObjectId,
    kind: ObjectKind,
    bytes: usize,
    encoded: Arc<Vec<u8>>,
}

impl PlainRepresentation {
    pub(crate) fn from_ingress(
        oid: ObjectId,
        kind: ObjectKind,
        bytes: usize,
        encoded: Vec<u8>,
    ) -> Self {
        Self {
            oid,
            kind,
            bytes,
            encoded: Arc::new(encoded),
        }
    }

    pub fn oid(&self) -> ObjectId {
        self.oid
    }

    pub fn kind(&self) -> ObjectKind {
        self.kind
    }

    pub fn object_bytes(&self) -> usize {
        self.bytes
    }

    pub fn encoded_bytes(&self) -> usize {
        self.encoded.len()
    }

    pub(crate) fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    pub(crate) fn matches(&self, oid: ObjectId, kind: ObjectKind, data: &[u8]) -> bool {
        if self.oid != oid || self.kind != kind || self.bytes != data.len() {
            return false;
        }
        let mut decoder = alt_inflate::Inflater::new(alt_inflate::Format::Zlib);
        let mut chunk = [0u8; 65536];
        let (mut read, mut compared) = (0, 0);
        loop {
            // one byte more than is left, so that a longer content shows
            let room = (data.len() - compared + 1).min(chunk.len());
            let Ok(step) = decoder.inflate(&self.encoded[read..], &mut chunk[..room]) else {
                return false;
            };
            read += step.consumed;
            if data.get(compared..compared + step.produced) != Some(&chunk[..step.produced]) {
                return false;
            }
            compared += step.produced;
            if step.done {
                return compared == data.len() && read == self.encoded.len();
            }
            if step.consumed == 0 && step.produced == 0 {
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::HashAlgo;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    fn encoded(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn representations_require_exact_current_bytes_and_one_complete_stream() {
        for data in [Vec::new(), vec![7; 65536 * 2 + 1]] {
            let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data);
            let compressed = encoded(&data);
            let value = PlainRepresentation::from_ingress(
                oid,
                ObjectKind::Blob,
                data.len(),
                compressed.clone(),
            );
            assert!(value.matches(oid, ObjectKind::Blob, &data));
            assert!(!value.matches(oid, ObjectKind::Tree, &data));
            assert!(!value.matches(
                ObjectId::hash_object(HashAlgo::Sha256, ObjectKind::Blob, &data),
                ObjectKind::Blob,
                &data,
            ));
            let mut changed = data.clone();
            if changed.is_empty() {
                changed.push(1);
            } else {
                changed[65536] ^= 1;
            }
            assert!(!value.matches(oid, ObjectKind::Blob, &changed));
            let mut trailing = compressed.clone();
            trailing.extend_from_slice(&encoded(b"extra"));
            let mut corrupt = compressed.clone();
            corrupt[0] ^= 1;
            for bad in [
                compressed[..compressed.len() - 1].to_vec(),
                trailing,
                corrupt,
            ] {
                let value =
                    PlainRepresentation::from_ingress(oid, ObjectKind::Blob, data.len(), bad);
                assert!(!value.matches(oid, ObjectKind::Blob, &data));
            }
        }
        // A tiny expected output must not allocate or accept the large stream.
        let value = PlainRepresentation::from_ingress(
            ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"x"),
            ObjectKind::Blob,
            1,
            encoded(&vec![0; 1024 * 1024]),
        );
        assert!(!value.matches(value.oid(), ObjectKind::Blob, b"x"));
    }
}
