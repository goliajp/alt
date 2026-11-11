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
            let mut rest = data;
            while !rest.is_empty() {
                let space = rest
                    .iter()
                    .position(|byte| *byte == b' ')
                    .ok_or(Error::Format("tree entry mode"))?;
                let mut mode = 0u32;
                for digit in &rest[..space] {
                    if !(b'0'..=b'7').contains(digit) || mode > 0o777777 {
                        return Err(Error::Format("tree entry mode"));
                    }
                    mode = mode << 3 | u32::from(digit - b'0');
                }
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
                match mode >> 12 {
                    0o04 => entries.push((oid, Kind::Tree)),
                    // a gitlink names a commit of another repository
                    0o16 => {}
                    0o10 | 0o12 => entries.push((oid, Kind::Blob)),
                    _ => return Err(Error::Format("tree entry mode")),
                }
            }
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
