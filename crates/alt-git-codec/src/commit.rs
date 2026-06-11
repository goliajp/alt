use bstr::{BStr, BString};

use crate::headers::HeaderBlock;
use crate::{ObjectId, ObjectParseError};

/// A parsed commit object.
///
/// Internally a [`HeaderBlock`], so unknown headers (`gpgsig`, `mergetag`,
/// `encoding`, …) survive byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    block: HeaderBlock,
}

impl Commit {
    pub fn parse(data: &[u8]) -> Result<Self, ObjectParseError> {
        HeaderBlock::parse(data).map(|block| Self { block })
    }

    pub fn serialize_into(&self, out: &mut Vec<u8>) {
        self.block.serialize_into(out);
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.serialize_into(&mut out);
        out
    }

    pub fn tree(&self) -> Option<ObjectId> {
        self.block
            .value(b"tree")
            .and_then(|v| ObjectId::from_hex(v).ok())
    }

    pub fn parents(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.block
            .values(b"parent")
            .filter_map(|v| ObjectId::from_hex(v).ok())
    }

    /// Raw `author` / `committer` ident lines (`name <email> time tz`).
    pub fn author(&self) -> Option<&BStr> {
        self.block.value(b"author")
    }

    pub fn committer(&self) -> Option<&BStr> {
        self.block.value(b"committer")
    }

    pub fn message(&self) -> &BString {
        &self.block.message
    }

    /// All headers in original order.
    pub fn headers(&self) -> &[(BString, BString)] {
        &self.block.headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashAlgo;

    const SAMPLE: &[u8] = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
        parent e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n\
        parent 3b18e512dba79e4c8300dd08aeb37f8e728b8dad\n\
        author A U Thor <a@example.com> 1700000000 +0900\n\
        committer A U Thor <a@example.com> 1700000001 +0900\n\
        \n\
        merge!\n";

    #[test]
    fn accessors_and_round_trip() {
        let commit = Commit::parse(SAMPLE).unwrap();
        assert_eq!(
            commit.tree().unwrap().to_string(),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        );
        assert_eq!(commit.parents().count(), 2);
        assert!(commit.author().unwrap().starts_with(b"A U Thor"));
        assert_eq!(commit.message().as_slice(), b"merge!\n");
        assert_eq!(commit.serialize(), SAMPLE);
        // and the serialization hashes back to the same id as the input
        assert_eq!(
            ObjectId::hash_object(
                HashAlgo::Sha1,
                crate::ObjectKind::Commit,
                &commit.serialize()
            ),
            ObjectId::hash_object(HashAlgo::Sha1, crate::ObjectKind::Commit, SAMPLE),
        );
    }
}
