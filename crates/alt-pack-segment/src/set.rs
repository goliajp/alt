//! The live segments of one repository, searched newest first.

use crate::{
    CommitInfo, Error, Kind, Oid, Record, Segment,
    format::Delta,
    scan::{self, Form},
};
use flate2::Decompress;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct SegmentSet {
    segments: Vec<Arc<Segment>>,
}

impl SegmentSet {
    /// `segments` in publication order
    pub fn new(segments: Vec<Arc<Segment>>) -> Self {
        Self { segments }
    }

    pub fn segments(&self) -> &[Arc<Segment>] {
        &self.segments
    }

    pub fn objects(&self) -> u64 {
        self.segments.iter().map(|segment| segment.len() as u64).sum()
    }

    pub(crate) fn locate(&self, oid: &Oid) -> Option<(&Segment, usize)> {
        self.segments
            .iter()
            .rev()
            .find_map(|segment| segment.find(oid).map(|index| (segment.as_ref(), index)))
    }

    pub fn record(&self, oid: &Oid) -> Result<Option<Record>, Error> {
        self.locate(oid).map(|(segment, index)| segment.record(index)).transpose()
    }

    pub fn kind(&self, oid: &Oid) -> Result<Option<Kind>, Error> {
        Ok(self.record(oid)?.map(|record| record.kind))
    }

    pub fn commit(&self, oid: &Oid) -> Option<CommitInfo> {
        self.segments.iter().rev().find_map(|segment| segment.commit(oid))
    }

    /// the canonical bytes of an object, resolving its delta chain
    pub fn read(&self, oid: &Oid) -> Result<(Kind, Vec<u8>), Error> {
        let mut engine = Decompress::new(true);
        let mut deltas = Vec::new();
        let (mut segment, mut index) = self.locate(oid).ok_or(Error::NotFound(*oid))?;
        let kind = segment.record(index)?.kind;
        let mut data = loop {
            let record = segment.record(index)?;
            let entry = segment.entry(&record)?;
            let (form, size, head) = scan::header(entry, 0)?;
            let (payload, _) = scan::inflate(&mut engine, &entry[head..], size as usize)?;
            match (record.delta, form) {
                (Delta::Plain, Form::Plain(_)) => break payload,
                (Delta::Inside, Form::Offset(_) | Form::Reference(_)) => {
                    deltas.push(payload);
                    index = record.base as usize;
                }
                (Delta::Outside, Form::Reference(base)) => {
                    deltas.push(payload);
                    (segment, index) = self.locate(&base).ok_or(Error::MissingBase(base))?;
                }
                _ => return Err(Error::Corrupt("record and entry disagree")),
            }
            if deltas.len() > 4096 {
                return Err(Error::Corrupt("delta chain"));
            }
        };
        for delta in deltas.iter().rev() {
            data = alt_git_pack::delta::apply(&data, delta).map_err(|_| Error::Corrupt("delta"))?;
        }
        Ok((kind, data))
    }
}
