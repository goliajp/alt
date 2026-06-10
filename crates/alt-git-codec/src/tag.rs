use bstr::{BStr, BString};

use crate::headers::HeaderBlock;
use crate::{ObjectId, ObjectKind, ObjectParseError};

/// A parsed annotated-tag object. Same text layout as [`crate::Commit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    block: HeaderBlock,
}

impl Tag {
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

    /// The tagged object id (`object` header).
    pub fn object(&self) -> Option<ObjectId> {
        self.block
            .value(b"object")
            .and_then(|v| ObjectId::from_hex(v).ok())
    }

    /// The tagged object's kind (`type` header).
    pub fn target_kind(&self) -> Option<ObjectKind> {
        self.block
            .value(b"type")
            .and_then(|v| ObjectKind::from_bytes(v).ok())
    }

    /// The tag name (`tag` header).
    pub fn name(&self) -> Option<&BStr> {
        self.block.value(b"tag")
    }

    pub fn tagger(&self) -> Option<&BStr> {
        self.block.value(b"tagger")
    }

    pub fn message(&self) -> &BString {
        &self.block.message
    }

    pub fn headers(&self) -> &[(BString, BString)] {
        &self.block.headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"object 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
        type commit\n\
        tag v1.0\n\
        tagger A U Thor <a@example.com> 1700000000 +0900\n\
        \n\
        release\n";

    #[test]
    fn accessors_and_round_trip() {
        let tag = Tag::parse(SAMPLE).unwrap();
        assert_eq!(tag.target_kind(), Some(ObjectKind::Commit));
        assert_eq!(tag.name().unwrap().as_ref() as &[u8], b"v1.0");
        assert_eq!(tag.serialize(), SAMPLE);
    }
}
