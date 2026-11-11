//! Writing a segment object by object, for content that did not arrive as a pack.
//!
//! Each object is compressed and written as it is handed over, and only its record
//! is kept: what a build holds in memory grows with the number of objects, not with
//! their bytes. References are settled as the objects come, in any order, and the
//! index follows the pack when the last one is in.
//!
//! The objects' other copies are let go once the segment is published, so a build
//! ends by reading every entry back from where it was written: nothing comes out of
//! `finish` that has not been inflated and compared with what was handed over.

use crate::{
    Error, FLAG_DUPLICATES, FLAG_OVERLAP, Kind, Oid, Record, SegmentSet,
    format::{self, CommitRow, Delta, NO_BASE},
    object::{self, Parsed},
    object_id,
    send::{entry_header, pack_header},
};
use std::{
    collections::HashMap,
    io::{BufReader, Read, Seek, SeekFrom, Write},
};

/// the zlib stream of one object's bytes
pub type Compress = fn(&[u8]) -> Vec<u8>;

fn deflate(data: &[u8]) -> Vec<u8> {
    alt_deflate::compress(data, 1, alt_deflate::Format::Zlib)
}

pub struct Builder<'a, W> {
    out: W,
    /// where the pack begins in `out`
    start: u64,
    compress: Compress,
    existing: &'a SegmentSet,
    pack: alt_hash::Sha1,
    written: u64,
    declared: usize,
    records: Vec<Record>,
    commits: Vec<CommitRow>,
    seen: HashMap<Oid, Kind>,
    /// referenced and not handed over yet: the kind expected and one object that refers to it
    wanted: HashMap<Oid, (Kind, Oid)>,
    flags: u32,
}

/// what `Builder::finish` wrote
pub struct Built<W> {
    pub out: W,
    /// distinct objects
    pub objects: u64,
    /// length of the segment: pack and index
    pub bytes: u64,
    pub flags: u32,
}

impl<'a, W: Read + Write + Seek> Builder<'a, W> {
    /// A segment of exactly `objects` objects, written to `out` from its current
    /// position. `existing` are the segments already present: what the new objects
    /// refer to is looked for there and among each other.
    pub fn new(out: W, existing: &'a SegmentSet, objects: u64) -> Result<Self, Error> {
        Self::compressing(out, existing, objects, deflate)
    }

    /// `new` with another compressor. What it returns is written as it is, and
    /// `finish` fails unless every stream reads back as the object it was given.
    pub fn compressing(
        mut out: W,
        existing: &'a SegmentSet,
        objects: u64,
        compress: Compress,
    ) -> Result<Self, Error> {
        let start = out.stream_position()?;
        let mut builder = Self {
            out,
            start,
            compress,
            existing,
            pack: alt_hash::Sha1::new(),
            written: 0,
            declared: objects as usize,
            records: Vec::with_capacity(objects as usize),
            commits: Vec::new(),
            seen: HashMap::with_capacity(objects as usize),
            wanted: HashMap::new(),
            flags: 0,
        };
        builder.put(&pack_header(objects)?)?;
        Ok(builder)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.pack.update(bytes);
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    fn refer(&mut self, from: &Oid, to: Oid, kind: Kind) -> Result<(), Error> {
        let found = match self.seen.get(&to) {
            Some(found) => Some(*found),
            None => self.existing.kind(&to)?,
        };
        match found {
            Some(found) if found == kind => Ok(()),
            Some(_) => Err(Error::Dangling(*from, to)),
            None => match self.wanted.insert(to, (kind, *from)) {
                Some((expected, _)) if expected != kind => Err(Error::Dangling(*from, to)),
                _ => Ok(()),
            },
        }
    }

    /// Appends one whole object. Its id is worked out here; handing over the same
    /// object twice stores it twice and marks the segment as holding duplicates.
    pub fn add(&mut self, kind: Kind, data: &[u8]) -> Result<(), Error> {
        if self.records.len() == self.declared {
            return Err(Error::Format("more objects than declared"));
        }
        let oid = object_id(kind, data);
        let header = entry_header(kind as u8, data.len() as u64);
        let compressed = (self.compress)(data);
        let mut crc = alt_hash::Crc32::new();
        crc.update(&header);
        crc.update(&compressed);
        let offset = self.written;
        self.put(&header)?;
        self.put(&compressed)?;
        self.records.push(Record {
            oid,
            blake3: *alt_hash::blake3::hash(data).as_bytes(),
            offset,
            size: data.len() as u64,
            crc: crc.finalize(),
            len: u32::try_from(header.len() + compressed.len())
                .map_err(|_| Error::Limit("entry bytes"))?,
            base: NO_BASE,
            kind,
            delta: Delta::Plain,
            depth: 0,
        });
        if self.seen.insert(oid, kind).is_some() {
            self.flags |= FLAG_DUPLICATES;
            return Ok(());
        }
        if let Some((expected, from)) = self.wanted.remove(&oid)
            && expected != kind
        {
            return Err(Error::Dangling(from, oid));
        }
        match object::parse(kind, data)? {
            Parsed::Blob => {}
            Parsed::Commit { tree, parents } => {
                self.refer(&oid, tree, Kind::Tree)?;
                for parent in &parents {
                    self.refer(&oid, *parent, Kind::Commit)?;
                }
                self.commits.push(CommitRow {
                    oid,
                    tree,
                    generation: 0,
                    parents,
                });
            }
            Parsed::Tree(entries) => {
                for (to, kind) in entries {
                    self.refer(&oid, to, kind)?;
                }
            }
            Parsed::Tag { object, kind } => self.refer(&oid, object, kind)?,
        }
        Ok(())
    }

    /// Ends the pack, reads every entry back and appends the index. Fails if the
    /// declared number of objects did not come, if one of them refers to an object
    /// neither the segment nor `existing` holds, or if an entry does not read back as
    /// the object it was written for.
    pub fn finish(mut self) -> Result<Built<W>, Error> {
        if self.records.len() != self.declared {
            return Err(Error::Format("fewer objects than declared"));
        }
        if let Some((to, (_, from))) = self.wanted.iter().min_by_key(|(to, _)| **to) {
            return Err(Error::Dangling(*from, *to));
        }
        let trailer = self.pack.finalize();
        self.out.write_all(&trailer)?;
        let pack_len = self.written + 20;
        self.out.seek(SeekFrom::Start(self.start + 12))?;
        // before duplicates are dropped: every entry written is read
        crate::readback::entries(
            &mut BufReader::with_capacity(256 << 10, &mut self.out),
            &self.records,
        )?;
        self.out.seek(SeekFrom::Start(self.start + pack_len))?;
        // sorted by oid, the first occurrence wins
        self.records
            .sort_by(|a, b| a.oid.cmp(&b.oid).then(a.offset.cmp(&b.offset)));
        self.records.dedup_by(|later, first| later.oid == first.oid);
        if self
            .records
            .iter()
            .any(|record| self.existing.locate_any(&record.oid))
        {
            self.flags |= FLAG_OVERLAP;
        }
        self.commits.sort_by_key(|commit| commit.oid);
        crate::generation::assign(&mut self.commits, self.existing)?;
        let mut index = Vec::new();
        format::write_index(
            &self.records,
            &self.commits,
            self.flags,
            pack_len,
            &mut index,
        )?;
        self.out.write_all(&index)?;
        Ok(Built {
            out: self.out,
            objects: self.records.len() as u64,
            bytes: pack_len + index.len() as u64,
            flags: self.flags,
        })
    }
}
