//! One object read as a stream.

use crate::{
    Error, Kind, Oid, Segment, SegmentSet,
    format::Delta,
    scan::{self, Form},
};
use std::{
    io::{self, Read},
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

/// The memory an object stored as a delta is restored in, and how long it
/// waits for its turn there: until `by`, and while `gone` is not set.
#[derive(Clone)]
pub struct Restoring {
    pub memory: &'static crate::Memory,
    pub by: Option<Instant>,
    pub gone: Option<Arc<AtomicBool>>,
}

/// the compressed bytes of one stored entry
struct Stored {
    segment: Arc<Segment>,
    at: usize,
    end: usize,
}

impl Read for Stored {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(self.end - self.at);
        output[..count].copy_from_slice(&self.segment.pack()[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

enum Source {
    Stored(Box<alt_inflate::Reader<Stored>>),
    Whole(io::Cursor<Vec<u8>>),
}

/// The canonical bytes of an object. It keeps the segment it reads from mapped, so it
/// outlives the set it came from and a merge that retires the segment.
pub struct ObjectStream {
    source: Source,
    /// what an object restored whole takes of the memory it was restored in
    _held: Option<alt_git_pack::Taken<'static>>,
}

impl Read for ObjectStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match &mut self.source {
            Source::Stored(reader) => reader.read(output),
            Source::Whole(data) => data.read(output),
        }
    }
}

impl SegmentSet {
    /// Kind, size and bytes of an object. One stored whole is inflated as it is
    /// read; one stored as a delta is restored first.
    pub fn open(&self, oid: &Oid) -> Result<Option<(Kind, u64, ObjectStream)>, Error> {
        self.open_within(oid, None)
    }

    /// [`Self::open`], restoring an object stored as a delta in `room` where
    /// one is given: what the restoring holds at once is taken of it before
    /// and given back once the stream is dropped, all of it where it asks for
    /// more than there is.
    pub fn open_within(
        &self,
        oid: &Oid,
        room: Option<&Restoring>,
    ) -> Result<Option<(Kind, u64, ObjectStream)>, Error> {
        let found = self
            .segments()
            .iter()
            .rev()
            .find_map(|segment| segment.find(oid).map(|index| (segment, index)));
        let Some((segment, index)) = found else {
            return Ok(None);
        };
        let record = segment.record(index)?;
        let mut held = None;
        let source = if record.delta == Delta::Plain {
            let entry = segment.entry(&record)?;
            let (form, _, head) = scan::header(entry, 0)?;
            if !matches!(form, Form::Plain(_)) {
                return Err(Error::Corrupt("record and entry disagree"));
            }
            let start = record.offset as usize;
            Source::Stored(Box::new(alt_inflate::Reader::new(
                alt_inflate::Format::Zlib,
                Stored {
                    segment: segment.clone(),
                    at: start + head,
                    end: start + record.len as usize,
                },
            )))
        } else {
            if let Some(room) = room {
                let place = self.place(oid).ok_or(Error::NotFound(*oid))?;
                let bytes = self.restored_bytes(place)?.min(room.memory.all());
                let wait = crate::Wait::new(room.by, room.gone.as_deref());
                held = Some(room.memory.let_in(bytes, 0, wait)?);
            }
            Source::Whole(io::Cursor::new(self.read(oid)?.1))
        };
        let stream = ObjectStream {
            source,
            _held: held,
        };
        Ok(Some((record.kind, record.size, stream)))
    }

    /// The most the object at `(at, index)` holds at once while it is restored:
    /// the deltas of its chain, read before any is applied, and an object and
    /// the next made of it, as large as the largest of the chain.
    pub fn restored_bytes(&self, (mut at, mut index): (usize, usize)) -> Result<u64, Error> {
        let (mut deltas, mut largest) = (0u64, 0u64);
        for _ in 0..=4096 {
            let segment = &self.segments()[at];
            let record = segment.record(index)?;
            largest = largest.max(record.size);
            let (form, size, _) = scan::header(segment.entry(&record)?, 0)?;
            match (record.delta, form) {
                (Delta::Plain, Form::Plain(_)) => {
                    return Ok(deltas.saturating_add(largest.saturating_mul(2)));
                }
                (Delta::Inside, Form::Offset(_) | Form::Reference(_)) => {
                    index = record.base as usize
                }
                (Delta::Outside, Form::Reference(base)) => {
                    (at, index) = self.base(at, &base).ok_or(Error::MissingBase(base))?;
                }
                _ => return Err(Error::Corrupt("record and entry disagree")),
            }
            deltas = deltas.saturating_add(size);
        }
        Err(Error::Corrupt("delta chain"))
    }

    /// `open` for the object whose content has this BLAKE3 digest
    pub fn open_blake3(
        &self,
        digest: &[u8; 32],
    ) -> Result<Option<(Oid, Kind, u64, ObjectStream)>, Error> {
        self.open_blake3_within(digest, None)
    }

    /// [`Self::open_within`] for the object whose content has this BLAKE3 digest
    pub fn open_blake3_within(
        &self,
        digest: &[u8; 32],
        room: Option<&Restoring>,
    ) -> Result<Option<(Oid, Kind, u64, ObjectStream)>, Error> {
        let found = self.segments().iter().rev().find_map(|segment| {
            segment
                .find_blake3(digest)
                .map(|index| segment.record(index))
        });
        let Some(record) = found.transpose()? else {
            return Ok(None);
        };
        Ok(self
            .open_within(&record.oid, room)?
            .map(|(kind, size, stream)| (record.oid, kind, size, stream)))
    }
}
