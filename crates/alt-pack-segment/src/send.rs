//! Producing packs from stored segments: whole segments verbatim, or a chosen object set
//! whose compressed entries are copied without recompression.

use crate::{
    Error, Kind, Oid, SegmentSet,
    format::Delta,
    scan::{self, Form},
    select::Selection,
};
use std::{collections::HashSet, io::Write};

struct Hashing<'a, W: Write> {
    inner: &'a mut W,
    hasher: alt_hash::Sha1,
}

impl<W: Write> Hashing<'_, W> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.hasher.update(bytes);
        self.inner.write_all(bytes)?;
        Ok(())
    }

    fn finish(self) -> Result<(), Error> {
        let trailer = self.hasher.finalize();
        self.inner.write_all(&trailer)?;
        Ok(())
    }
}

pub(crate) fn pack_header(count: u64) -> Result<[u8; 12], Error> {
    let count = u32::try_from(count).map_err(|_| Error::Limit("objects in one pack"))?;
    let mut header = *b"PACK\0\0\0\x02\0\0\0\0";
    header[8..].copy_from_slice(&count.to_be_bytes());
    Ok(header)
}

pub(crate) fn entry_header(code: u8, mut size: u64) -> Vec<u8> {
    let mut out = vec![code << 4 | (size & 15) as u8];
    size >>= 4;
    while size != 0 {
        *out.last_mut().unwrap() |= 0x80;
        out.push((size & 0x7f) as u8);
        size >>= 7;
    }
    out
}

/// a self-contained pack of whole objects, for content that did not arrive as a pack
pub fn build_pack<'a>(
    objects: impl ExactSizeIterator<Item = (Kind, &'a [u8])>,
) -> Result<Vec<u8>, Error> {
    let mut pack = pack_header(objects.len() as u64)?.to_vec();
    for (kind, data) in objects {
        pack.extend_from_slice(&entry_header(kind as u8, data.len() as u64));
        pack.extend_from_slice(&alt_deflate::compress(data, 1, alt_deflate::Format::Zlib));
    }
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    Ok(pack)
}

impl SegmentSet {
    /// every segment can be sent as stored: no duplicate objects within or across segments
    pub fn verbatim(&self) -> bool {
        self.segments().iter().all(|segment| segment.flags() == 0)
    }

    /// all objects of all segments as one pack; entries are copied as stored
    pub fn write_all(&self, out: &mut impl Write) -> Result<(), Error> {
        if let [only] = self.segments() {
            out.write_all(only.pack())?;
            return Ok(());
        }
        let mut out = Hashing {
            inner: out,
            hasher: alt_hash::Sha1::new(),
        };
        out.put(&pack_header(self.objects())?)?;
        for segment in self.segments() {
            let pack = segment.pack();
            out.put(&pack[12..pack.len() - 20])?;
        }
        out.finish()
    }

    /// writes `selection` as one pack; with `thin`, deltas may rest on objects the client holds
    pub fn write_selected(
        &self,
        selection: &Selection,
        thin: bool,
        out: &mut impl Write,
    ) -> Result<(), Error> {
        let chosen: HashSet<&Oid> = selection.objects.iter().collect();
        let mut out = Hashing {
            inner: out,
            hasher: alt_hash::Sha1::new(),
        };
        out.put(&pack_header(selection.objects.len() as u64)?)?;
        for oid in &selection.objects {
            let (segment, index) = self.locate(oid).ok_or(Error::NotFound(*oid))?;
            let record = segment.record(index)?;
            let entry = segment.entry(&record)?;
            if record.delta == Delta::Plain {
                out.put(entry)?;
                continue;
            }
            let (form, size, head) = scan::header(entry, 0)?;
            let base = match (record.delta, form) {
                (Delta::Inside, _) => segment.record(record.base as usize)?.oid,
                (_, Form::Reference(base)) => base,
                _ => return Err(Error::Corrupt("record and entry disagree")),
            };
            if chosen.contains(&base) || (thin && selection.known.contains(&base)) {
                out.put(&entry_header(7, size))?;
                out.put(&base)?;
                out.put(&entry[head..])?;
                continue;
            }
            let (kind, data) = self.read(oid)?;
            out.put(&entry_header(kind as u8, data.len() as u64))?;
            out.put(&alt_deflate::compress(&data, 1, alt_deflate::Format::Zlib))?;
        }
        out.finish()
    }
}
