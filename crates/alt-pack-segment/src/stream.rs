//! One object read as a stream.

use crate::{
    Error, Kind, Oid, Segment, SegmentSet,
    format::Delta,
    scan::{self, Form},
};
use std::{
    io::{self, Read},
    sync::Arc,
};

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
        let found = self
            .segments()
            .iter()
            .rev()
            .find_map(|segment| segment.find(oid).map(|index| (segment, index)));
        let Some((segment, index)) = found else {
            return Ok(None);
        };
        let record = segment.record(index)?;
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
            Source::Whole(io::Cursor::new(self.read(oid)?.1))
        };
        Ok(Some((record.kind, record.size, ObjectStream { source })))
    }

    /// `open` for the object whose content has this BLAKE3 digest
    pub fn open_blake3(
        &self,
        digest: &[u8; 32],
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
            .open(&record.oid)?
            .map(|(kind, size, stream)| (record.oid, kind, size, stream)))
    }
}
