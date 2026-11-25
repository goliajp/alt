//! Every object of a segment read once, each delta applied to its base once.
//!
//! Reading the objects one by one makes each of its whole chain: a chain of
//! four thousand deltas is eight million deltas applied. Here the deltas of a
//! segment are gone through from their bases downwards, and an object is kept
//! while the deltas made of it are read. What is kept at once has a bound; a
//! base let go for it is read again, by its chain, where a delta of it is come
//! to after that.

use crate::{
    Error, Oid, Record, SegmentSet,
    format::Delta,
    scan::{self, Form},
};

/// the most deltas a read goes down
const DEEPEST: u32 = 4096;

/// an object on the way from a base to the delta that is read
struct Above {
    index: u32,
    /// its deltas yet to read, among those of the segment
    next: u32,
    end: u32,
    /// the deltas below it
    depth: u32,
    /// none where its read failed, or where it was let go
    data: Option<Vec<u8>>,
    /// what its read ended in, and with it the read of all that is made of it
    failed: Option<Error>,
}

/// the base of another segment last read, with the deltas below it
type Outside = Option<((usize, usize), u32, Vec<u8>)>;

impl SegmentSet {
    /// the deltas a read of the record at `place` goes down, counted no further
    /// than one beyond [`DEEPEST`]
    fn deltas_below(&self, (mut at, mut index): (usize, usize)) -> Result<u32, Error> {
        let mut deltas = 0;
        while deltas <= DEEPEST {
            let segment = &self.segments()[at];
            let record = segment.record(index)?;
            match record.delta {
                Delta::Plain => break,
                Delta::Inside => index = record.base as usize,
                Delta::Outside => {
                    let (form, ..) = scan::header(segment.entry(&record)?, 0)?;
                    let Form::Reference(base) = form else {
                        return Err(Error::Corrupt("record and entry disagree"));
                    };
                    (at, index) = self.base(at, &base).ok_or(Error::MissingBase(base))?;
                }
            }
            deltas += 1;
        }
        Ok(deltas)
    }

    /// What the entry of a record inflates to, where it is of the form its
    /// record tells; and the base it names, where it names one.
    fn inflated(&self, at: usize, record: &Record) -> Result<(Vec<u8>, Option<Oid>), Error> {
        let entry = self.segments()[at].entry(record)?;
        let (form, size, head) = scan::header(entry, 0)?;
        let named = match (record.delta, form) {
            (Delta::Plain, Form::Plain(_)) => None,
            (Delta::Inside, Form::Offset(_) | Form::Reference(_)) => None,
            (Delta::Outside, Form::Reference(base)) => Some(base),
            _ => return Err(Error::Corrupt("record and entry disagree")),
        };
        Ok((scan::inflate(&entry[head..], size as usize)?.0, named))
    }

    /// The object of a record that is made of nothing in its own segment, with
    /// the deltas below it.
    fn begun(
        &self,
        at: usize,
        record: &Record,
        (last, applied): (&mut Outside, &mut u64),
    ) -> Result<(Vec<u8>, u32), Error> {
        let (payload, named) = self.inflated(at, record)?;
        let Some(base) = named else {
            return match record.delta {
                Delta::Plain => Ok((payload, 0)),
                _ => Err(Error::Corrupt("delta base")),
            };
        };
        let place = self.base(at, &base).ok_or(Error::MissingBase(base))?;
        if !matches!(last, Some((had, ..)) if *had == place) {
            let below = self.deltas_below(place)?;
            if below >= DEEPEST {
                return Err(Error::Corrupt("delta chain"));
            }
            *last = Some((place, below, self.read_at(place)?.1));
            *applied += u64::from(below);
        }
        let (_, below, data) = last.as_ref().expect("the base just read");
        *applied += 1;
        let made = alt_git_pack::delta::apply(data, &payload);
        Ok((made.map_err(|_| Error::Corrupt("delta"))?, below + 1))
    }

    /// Tells `told` of every object of the newest segment of the set: the
    /// record, and what a read of the object gives or ends in. No more than
    /// `held` bytes of objects are kept for the deltas made of them, beside
    /// the one that is read. The deltas that were applied.
    pub(crate) fn each_of_the_newest(
        &self,
        held: usize,
        told: impl FnMut(&Record, Result<&[u8], Error>),
    ) -> Result<u64, Error> {
        let at = self.segments().len() - 1;
        let segment = &self.segments()[at];
        let count = segment.len();
        // the deltas of each record, one list behind where those of each begin
        let mut starts = vec![0u32; count + 1];
        let mut begin = Vec::new();
        let inside = |record: &Record| {
            (record.delta == Delta::Inside && (record.base as usize) < count)
                .then_some(record.base as usize)
        };
        for index in 0..count {
            match inside(&segment.record(index)?) {
                Some(base) => starts[base + 1] += 1,
                None => begin.push(index as u32),
            }
        }
        for index in 0..count {
            starts[index + 1] += starts[index];
        }
        let mut deltas = vec![0u32; count - begin.len()];
        let mut filled = starts.clone();
        for index in 0..count {
            if let Some(base) = inside(&segment.record(index)?) {
                deltas[filled[base] as usize] = index as u32;
                filled[base] += 1;
            }
        }
        drop(filled);
        let mut reading = Reading {
            set: self,
            at,
            starts,
            deltas,
            read: vec![false; count],
            way: Vec::new(),
            kept: 0,
            let_go: 0,
            held,
            applied: 0,
            told,
        };
        let mut last: Outside = None;
        for first in begin {
            let record = segment.record(first as usize)?;
            let begun = self.begun(at, &record, (&mut last, &mut reading.applied));
            reading.come_to(first, &record, begun);
            reading.below()?;
        }
        // made of one another, round in a circle: no base leads to them
        for index in (0..count).filter(|index| !reading.read[*index]) {
            let circle = Err(Error::Corrupt("delta chain"));
            (reading.told)(&segment.record(index)?, circle);
        }
        Ok(reading.applied)
    }
}

struct Reading<'a, F> {
    set: &'a SegmentSet,
    /// the segment that is read, by its place in the set
    at: usize,
    /// the deltas of the record `n` are `deltas[starts[n]..starts[n + 1]]`
    starts: Vec<u32>,
    deltas: Vec<u32>,
    read: Vec<bool>,
    way: Vec<Above>,
    /// the bytes kept on the way, and how far up the way they were let go
    kept: usize,
    let_go: usize,
    held: usize,
    /// the deltas applied
    applied: u64,
    told: F,
}

impl<F: FnMut(&Record, Result<&[u8], Error>)> Reading<'_, F> {
    /// Tells of the object `index`, read as `made` with so many deltas below
    /// it, and puts it on the way for its own deltas.
    fn come_to(&mut self, index: u32, record: &Record, made: Result<(Vec<u8>, u32), Error>) {
        self.read[index as usize] = true;
        (self.told)(
            record,
            made.as_ref().map(|(data, _)| &data[..]).map_err(again),
        );
        let depth = made.as_ref().map_or(0, |made| made.1);
        let (data, failed) = match made {
            Ok(made) => (Some(made.0), None),
            Err(error) => (None, Some(error)),
        };
        self.kept += data.as_ref().map_or(0, Vec::len);
        self.way.push(Above {
            index,
            next: self.starts[index as usize],
            end: self.starts[index as usize + 1],
            depth,
            data,
            failed,
        });
        while self.kept > self.held && self.let_go + 1 < self.way.len() {
            let gone = self.way[self.let_go].data.take();
            self.kept -= gone.map_or(0, |data| data.len());
            self.let_go += 1;
        }
    }

    /// Reads everything below the object the way begins with.
    fn below(&mut self) -> Result<(), Error> {
        while let Some(above) = self.way.last_mut() {
            if above.next == above.end {
                self.kept -= above.data.as_ref().map_or(0, Vec::len);
                self.way.pop();
                self.let_go = self.let_go.min(self.way.len());
                continue;
            }
            let index = self.deltas[above.next as usize];
            above.next += 1;
            let record = self.set.segments()[self.at].record(index as usize)?;
            let depth = above.depth + 1;
            let made = if let Some(failed) = &above.failed {
                Err(again(failed))
            } else if depth > DEEPEST {
                Err(Error::Corrupt("delta chain"))
            } else {
                if above.data.is_none() {
                    let again = self.set.read_at((self.at, above.index as usize))?.1;
                    self.applied += u64::from(above.depth);
                    self.kept += again.len();
                    above.data = Some(again);
                    self.let_go = self.way.len() - 1;
                }
                let base = self.way.last().and_then(|above| above.data.as_deref());
                let base = base.expect("the base just had");
                self.applied += 1;
                self.set.inflated(self.at, &record).and_then(|(delta, _)| {
                    alt_git_pack::delta::apply(base, &delta).map_err(|_| Error::Corrupt("delta"))
                })
            };
            self.come_to(index, &record, made.map(|made| (made, depth)));
        }
        self.let_go = 0;
        Ok(())
    }
}

/// the same error once more, for the one that is told of it
fn again(error: &Error) -> Error {
    match error {
        Error::Io(error) => Error::Io(std::io::Error::new(error.kind(), error.to_string())),
        Error::Format(what) => Error::Format(what),
        Error::Limit(what) => Error::Limit(what),
        Error::Dangling(from, to) => Error::Dangling(*from, *to),
        Error::MissingBase(base) => Error::MissingBase(*base),
        Error::Corrupt(what) => Error::Corrupt(what),
        Error::NotFound(oid) => Error::NotFound(*oid),
        Error::Busy(what) => Error::Busy(what),
        Error::GivenUp => Error::GivenUp,
        Error::Unsafe(oid, what) => Error::Unsafe(*oid, what),
    }
}
