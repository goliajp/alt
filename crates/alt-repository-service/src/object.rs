//! One object read and verified: its id, its size, its encoding, and the
//! objects it names.

use crate::{Error, GraphLimits, Node, ObjectReader, Result, TreeLink};
use alt_git_codec::{Commit, HashAlgo, ObjectId, ObjectKind, Tag, Tree};
use std::collections::BTreeSet;
use std::io::Read;

pub(crate) fn check_oid(oid: ObjectId, algo: HashAlgo) -> Result<()> {
    if oid.algo() != algo || oid.as_bytes().iter().all(|b| *b == 0) {
        return Err(Error::Integrity("object id algorithm or zero sentinel"));
    }
    Ok(())
}
fn parse_oid(bytes: &[u8], algo: HashAlgo) -> Result<ObjectId> {
    let oid = ObjectId::from_hex(bytes).map_err(|_| Error::Integrity("malformed object id"))?;
    check_oid(oid, algo)?;
    Ok(oid)
}
pub fn verify_object(
    oid: ObjectId,
    mut object: ObjectReader<'_>,
    limits: GraphLimits,
) -> Result<Node> {
    if object.bytes > limits.object_bytes {
        return Err(Error::Limit("object bytes"));
    }
    let structured = object.kind != ObjectKind::Blob;
    if structured && object.bytes > limits.structured_bytes as u64 {
        return Err(Error::Limit("structured object bytes"));
    }
    let mut data = Vec::new();
    let mut sha1 = alt_hash::Sha1::new();
    let mut sha256 = alt_hash::Sha256::new();
    let header = format!("{} {}\0", object.kind, object.bytes);
    sha1.update(header.as_bytes());
    sha256.update(header.as_bytes());
    let mut remaining = object.bytes;
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let n = object
            .reader
            .read(&mut buffer[..remaining.min(65536) as usize])?;
        if n == 0 {
            return Err(Error::Integrity("truncated object"));
        }
        let bytes = &buffer[..n];
        match oid.algo() {
            HashAlgo::Sha1 => sha1.update(bytes),
            HashAlgo::Sha256 => sha256.update(bytes),
        }
        if structured {
            data.extend_from_slice(bytes);
        }
        remaining -= n as u64;
    }
    if object.reader.read(&mut buffer[..1])? != 0 {
        return Err(Error::Integrity("object size mismatch"));
    }
    let actual = match oid.algo() {
        HashAlgo::Sha1 => ObjectId::Sha1(sha1.finalize()),
        HashAlgo::Sha256 => ObjectId::Sha256(sha256.finalize()),
    };
    if actual != oid {
        return Err(Error::Integrity("object hash mismatch"));
    }
    parse_node(object.kind, &data, oid.algo())
}
fn single_header<'a>(
    headers: &'a [(alt_bytes::BString, alt_bytes::BString)],
    name: &[u8],
) -> Result<&'a [u8]> {
    let mut values = headers
        .iter()
        .filter(|(key, _)| key.as_slice() == name)
        .map(|(_, value)| value.as_slice());
    let value = values
        .next()
        .ok_or(Error::Integrity("missing required header"))?;
    if values.next().is_some() {
        return Err(Error::Integrity("duplicate required header"));
    }
    Ok(value)
}
pub fn parse_node(kind: ObjectKind, data: &[u8], algo: HashAlgo) -> Result<Node> {
    Ok(match kind {
        ObjectKind::Blob => Node::Blob,
        ObjectKind::Commit => {
            let commit = Commit::parse(data).map_err(|_| Error::Integrity("commit encoding"))?;
            let headers = commit.headers();
            let tree = parse_oid(single_header(headers, b"tree")?, algo)?;
            single_header(headers, b"author")?;
            single_header(headers, b"committer")?;
            let parents = headers
                .iter()
                .filter(|(key, _)| key.as_slice() == b"parent")
                .map(|(_, value)| parse_oid(value, algo))
                .collect::<Result<Vec<_>>>()?;
            Node::Commit { tree, parents }
        }
        ObjectKind::Tag => {
            let tag = Tag::parse(data).map_err(|_| Error::Integrity("tag encoding"))?;
            let target = parse_oid(single_header(tag.headers(), b"object")?, algo)?;
            let kind = ObjectKind::from_bytes(single_header(tag.headers(), b"type")?)
                .map_err(|_| Error::Integrity("tag target kind"))?;
            if single_header(tag.headers(), b"tag")?.is_empty() {
                return Err(Error::Integrity("empty tag name"));
            }
            Node::Tag { target, kind }
        }
        ObjectKind::Tree => {
            let tree = Tree::parse(data, algo).map_err(|_| Error::Integrity("tree encoding"))?;
            let mut names = BTreeSet::new();
            let mut entries = Vec::new();
            for entry in tree.entries {
                let name = entry.name.to_vec();
                // the names a push is refused for
                if alt_git_codec::unsafe_name(&name).is_some() || !names.insert(name.clone()) {
                    return Err(Error::Integrity("unsafe or duplicate tree name"));
                }
                let mode = entry.mode.value();
                if !matches!(mode, 0o040000 | 0o100644 | 0o100755 | 0o120000 | 0o160000) {
                    return Err(Error::Integrity("unsupported tree mode"));
                }
                check_oid(entry.oid, algo)?;
                entries.push(TreeLink {
                    name,
                    mode,
                    oid: entry.oid,
                });
            }
            // a tree out of the order Git writes is taken as Git takes it: its
            // bytes, and so its id, are what they are
            Node::Tree(entries)
        }
    })
}
