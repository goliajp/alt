use crate::*;

const MAGIC: &[u8; 8] = b"ALTCAT02";
pub(crate) const OVERHEAD: usize = 16 + 32;
pub(crate) fn entry_size(e: &Entry) -> usize {
    48 + e.attributes.len()
        + e.git.iter().map(|g| 2 + g.oid.bytes().len()).sum::<usize>()
        + if matches!(e.location, Location::Segment { .. }) {
            56
        } else {
            0
        }
}
pub(crate) fn encode(entries: &[Entry], limit: usize) -> Result<Vec<u8>> {
    let size = entries
        .iter()
        .try_fold(OVERHEAD, |n, e| n.checked_add(entry_size(e)))
        .ok_or(Error::Limit("encoded bytes"))?;
    if size > limit {
        return Err(Error::Limit("encoded bytes"));
    }
    let mut out = Vec::with_capacity(size);
    out.extend(MAGIC);
    out.extend((entries.len() as u32).to_le_bytes());
    out.extend(((size - OVERHEAD) as u32).to_le_bytes());
    for e in entries {
        out.extend(e.digest);
        out.extend(e.bytes.to_le_bytes());
        out.push(e.roles);
        out.push(e.git.len() as u8);
        out.push(match e.location {
            Location::Loose => 0,
            Location::Segment { .. } => 1,
            Location::Pending => 2,
        });
        out.push(0);
        out.extend((e.attributes.len() as u32).to_le_bytes());
        for g in &e.git {
            out.push(g.kind as u8);
            out.push(g.oid.algorithm());
            out.extend(g.oid.bytes());
        }
        if let Location::Segment { id, index, offset } = e.location {
            out.extend(id);
            out.extend(index);
            out.extend(offset.to_le_bytes());
        }
        out.extend(&e.attributes);
    }
    let id = blake3::hash(&out);
    out.extend(id.as_bytes());
    Ok(out)
}

pub(crate) struct Cursor<'a>(pub &'a [u8]);
impl Cursor<'_> {
    pub fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let bytes = self.0.get(..N).ok_or(Error::Format("truncated"))?;
        let value = bytes.try_into().unwrap();
        self.0 = &self.0[N..];
        Ok(value)
    }
    pub fn byte(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
}

pub(crate) fn decode(bytes: &[u8], expected: &Digest, limits: Limits) -> Result<Vec<Entry>> {
    if bytes.len() < OVERHEAD || bytes.len() > limits.bytes {
        return Err(Error::Limit("encoded bytes"));
    }
    let end = bytes.len() - 32;
    if &bytes[end..] != expected || blake3::hash(&bytes[..end]).as_bytes() != expected {
        return Err(Error::Format("block identity"));
    }
    let mut input = Cursor(&bytes[..end]);
    if &input.take::<8>()? != MAGIC {
        return Err(Error::Format("version"));
    }
    let count = u32::from_le_bytes(input.take()?) as usize;
    let size = u32::from_le_bytes(input.take()?) as usize;
    if count > limits.members || size != input.0.len() || count > size / 48 {
        return Err(Error::Format("member count or extent"));
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let digest = input.take()?;
        let bytes = u64::from_le_bytes(input.take()?);
        let roles = input.byte()?;
        let aliases = input.byte()? as usize;
        let location = input.byte()?;
        if input.byte()? != 0 || aliases > 8 || location > 2 {
            return Err(Error::Format("member flags"));
        }
        let attribute_bytes = u32::from_le_bytes(input.take()?) as usize;
        let mut git = Vec::with_capacity(aliases);
        for _ in 0..aliases {
            let kind = match input.byte()? {
                0 => GitKind::Commit,
                1 => GitKind::Tree,
                2 => GitKind::Blob,
                3 => GitKind::Tag,
                _ => return Err(Error::Format("Git kind")),
            };
            let oid = match input.byte()? {
                1 => ObjectId::Sha1(input.take()?),
                2 => ObjectId::Sha256(input.take()?),
                _ => return Err(Error::Format("Git algorithm")),
            };
            git.push(GitIdentity { oid, kind });
        }
        let location = if location == 0 {
            Location::Loose
        } else if location == 2 {
            Location::Pending
        } else {
            Location::Segment {
                id: input.take()?,
                index: input.take()?,
                offset: u64::from_le_bytes(input.take()?),
            }
        };
        let attributes = input
            .0
            .get(..attribute_bytes)
            .ok_or(Error::Format("attribute extent"))?
            .to_vec();
        input.0 = &input.0[attribute_bytes..];
        entries.push(Entry {
            digest,
            bytes,
            roles,
            git,
            location,
            attributes,
        });
    }
    if !input.0.is_empty() {
        return Err(Error::Format("trailing bytes"));
    }
    Ok(entries)
}
