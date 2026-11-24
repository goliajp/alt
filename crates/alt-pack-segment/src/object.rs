//! The references a commit, tree or tag makes, parsed without allocation per field.

use crate::{Error, Kind, Oid};

#[derive(Debug, Clone)]
pub(crate) enum Parsed {
    Blob,
    Commit { tree: Oid, parents: Vec<Oid> },
    Tree(Vec<(Oid, Kind)>),
    Tag { object: Oid, kind: Kind },
}

fn hex_oid(text: &[u8]) -> Option<Oid> {
    if text.len() != 40 {
        return None;
    }
    let mut oid = [0u8; 20];
    for (index, slot) in oid.iter_mut().enumerate() {
        let high = (text[index * 2] as char).to_digit(16)?;
        let low = (text[index * 2 + 1] as char).to_digit(16)?;
        *slot = (high << 4 | low) as u8;
    }
    Some(oid)
}

fn line<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let rest = data.get(*pos..)?;
    let end = rest.iter().position(|byte| *byte == b'\n')?;
    *pos += end + 1;
    Some(&rest[..end])
}

pub(crate) fn parse(kind: Kind, data: &[u8]) -> Result<Parsed, Error> {
    match kind {
        Kind::Blob => Ok(Parsed::Blob),
        Kind::Commit => {
            let mut pos = 0;
            let tree = line(data, &mut pos)
                .and_then(|text| text.strip_prefix(b"tree "))
                .and_then(hex_oid)
                .ok_or(Error::Format("commit without tree"))?;
            let mut parents = Vec::new();
            while let Some(text) = line(data, &mut pos) {
                let Some(parent) = text.strip_prefix(b"parent ") else {
                    break;
                };
                parents.push(hex_oid(parent).ok_or(Error::Format("commit parent"))?);
            }
            Ok(Parsed::Commit { tree, parents })
        }
        Kind::Tree => {
            let mut entries = Vec::new();
            tree(data, &mut |oid, kind| {
                entries.push((oid, kind));
                Ok(())
            })?;
            Ok(Parsed::Tree(entries))
        }
        Kind::Tag => {
            let mut pos = 0;
            let object = line(data, &mut pos)
                .and_then(|text| text.strip_prefix(b"object "))
                .and_then(hex_oid)
                .ok_or(Error::Format("tag object"))?;
            let kind = match line(data, &mut pos).and_then(|text| text.strip_prefix(b"type ")) {
                Some(b"commit") => Kind::Commit,
                Some(b"tree") => Kind::Tree,
                Some(b"blob") => Kind::Blob,
                Some(b"tag") => Kind::Tag,
                _ => return Err(Error::Format("tag type")),
            };
            Ok(Parsed::Tag { object, kind })
        }
    }
}

/// Hands `each` every object a tree refers to, in the order of its entries. A tree
/// is as large as an object may be, and what it refers to is not gathered here.
pub(crate) fn tree(
    data: &[u8],
    each: &mut impl FnMut(Oid, Kind) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or(Error::Format("tree entry mode"))?;
        let names = names(&rest[..space]).ok_or(Error::Format("tree entry mode"))?;
        rest = &rest[space + 1..];
        let nul = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(Error::Format("tree entry name"))?;
        let name = &rest[..nul];
        if name.is_empty() || name.contains(&b'/') || rest.len() < nul + 21 {
            return Err(Error::Format("tree entry name"));
        }
        let oid: Oid = rest[nul + 1..nul + 21].try_into().unwrap();
        rest = &rest[nul + 21..];
        if let Some(kind) = names {
            each(oid, kind)?;
        }
    }
    Ok(())
}

/// What an entry of a tree names by its mode: a tree, a file, or with `Some(None)`
/// a commit of another repository, which is no object of this one. A mode is
/// the number it is written as, with zeros before it or without: Git reads
/// `040000` as the directory `40000` is, and warns of it at the most.
pub(crate) fn names(mode: &[u8]) -> Option<Option<Kind>> {
    let mut number = 0u32;
    for digit in mode {
        if !(b'0'..=b'7').contains(digit) || number > 0o777777 {
            return None;
        }
        number = number << 3 | u32::from(digit - b'0');
    }
    match number >> 12 {
        0o04 => Some(Some(Kind::Tree)),
        0o16 => Some(None),
        0o10 | 0o12 => Some(Some(Kind::Blob)),
        _ => None,
    }
}
