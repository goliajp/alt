//! Reading the trees of a history one after the other.
//!
//! A pack keeps the versions of a directory as a chain of deltas, and a walk over a
//! history reads version after version of the same chains. Read alone, each of them
//! is made of every version below it again. The trees made are kept while the walk
//! goes on, within a bound, so that a version is made of the one the walk read before.

use crate::{
    Error, Kind, Oid, SegmentSet,
    format::Delta,
    scan::{self, Form},
};
use std::{collections::HashMap, rc::Rc};

/// The bytes of trees one walk keeps: what a fetch takes in memory beside the
/// tables of what it found.
const KEPT: usize = 16 << 20;

/// an entry of a tree that names an object of this repository
pub(crate) struct Entry<'a> {
    pub name: &'a [u8],
    pub oid: Oid,
    pub tree: bool,
}

pub(crate) fn entries(data: &[u8]) -> Result<Vec<Entry<'_>>, Error> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let bad = || Error::Corrupt("tree entry");
        let space = rest.iter().position(|byte| *byte == b' ').ok_or_else(bad)?;
        let (mode, named) = (&rest[..space], &rest[space + 1..]);
        let nul = named.iter().position(|byte| *byte == 0).ok_or_else(bad)?;
        let oid = named.get(nul + 1..nul + 21).ok_or_else(bad)?;
        if mode.contains(&0) {
            return Err(bad());
        }
        let entry = |tree| Entry {
            name: &named[..nul],
            oid: oid.try_into().unwrap(),
            tree,
        };
        match mode {
            b"40000" => out.push(entry(true)),
            // a gitlink names a commit of another repository
            b"160000" => {}
            _ => out.push(entry(false)),
        }
        rest = &named[nul + 21..];
    }
    Ok(out)
}

/// The entries of the tree another is compared with, come to in the order the other
/// has its own.
///
/// Git writes the entries of a tree in one order, a directory where its name with a
/// slash after it would be. Two trees in that order are compared by going through
/// both once. Where a tree is in another order an entry of it may be passed and not
/// found again: what it names is then taken as changed, and sent where it need not
/// have been, never left out.
pub(crate) struct Beside<'a, 'b> {
    entries: &'b [Entry<'a>],
    at: usize,
}

/// the byte a name is ordered by where the other name goes on
fn after(entry: &Entry<'_>, len: usize) -> u8 {
    let end = if entry.tree { b'/' } else { 0 };
    entry.name.get(len).copied().unwrap_or(end)
}

impl<'a, 'b> Beside<'a, 'b> {
    pub fn new(entries: &'b [Entry<'a>]) -> Self {
        Beside { entries, at: 0 }
    }

    /// what the tree has at the name of `entry` as a thing of its kind, a directory
    /// or not
    pub fn of(&mut self, entry: &Entry<'_>) -> Option<Oid> {
        while let Some(old) = self.entries.get(self.at) {
            let shared = old.name.len().min(entry.name.len());
            let order = (old.name[..shared].cmp(&entry.name[..shared]))
                .then_with(|| after(old, shared).cmp(&after(entry, shared)));
            match order {
                std::cmp::Ordering::Less => self.at += 1,
                std::cmp::Ordering::Equal => return Some(old.oid),
                std::cmp::Ordering::Greater => break,
            }
        }
        None
    }
}

/// where an object is: the segment, by its place in the set, and the record
type Place = (usize, usize);

pub(crate) struct Trees<'a> {
    set: &'a SegmentSet,
    /// The trees read of late, and those read before them. When the first has half
    /// of what may be kept it becomes the second, and what was the second is let go
    /// of: a tree read again in time is moved back to the first.
    late: HashMap<Place, Rc<Vec<u8>>>,
    before: HashMap<Place, Rc<Vec<u8>>>,
    late_bytes: usize,
}

impl<'a> Trees<'a> {
    pub fn new(set: &'a SegmentSet) -> Self {
        Trees {
            set,
            late: HashMap::new(),
            before: HashMap::new(),
            late_bytes: 0,
        }
    }

    fn kept(&mut self, at: Place) -> Option<Rc<Vec<u8>>> {
        if let Some(data) = self.late.get(&at) {
            return Some(data.clone());
        }
        let data = self.before.remove(&at)?;
        self.keep(at, data.clone());
        Some(data)
    }

    fn keep(&mut self, at: Place, data: Rc<Vec<u8>>) {
        // one tree is not to be most of what is kept
        if data.len() > KEPT / 8 {
            return;
        }
        if self.late_bytes + data.len() > KEPT / 2 {
            self.before = std::mem::take(&mut self.late);
            self.late_bytes = 0;
        }
        self.late_bytes += data.len();
        self.late.insert(at, data);
    }

    /// the canonical bytes of the tree `oid`
    pub fn read(&mut self, oid: &Oid) -> Result<Rc<Vec<u8>>, Error> {
        let mut at = self.set.place(oid).ok_or(Error::NotFound(*oid))?;
        let mut deltas = Vec::new();
        let mut data = loop {
            if let Some(data) = self.kept(at) {
                break data;
            }
            let segment = &self.set.segments()[at.0];
            let record = segment.record(at.1)?;
            if record.kind != Kind::Tree {
                return Err(Error::Corrupt("tree expected"));
            }
            let entry = segment.entry(&record)?;
            let (form, size, head) = scan::header(entry, 0)?;
            let (payload, _) = scan::inflate(&entry[head..], size as usize)?;
            match (record.delta, form) {
                (Delta::Plain, Form::Plain(_)) => {
                    let data = Rc::new(payload);
                    self.keep(at, data.clone());
                    break data;
                }
                (Delta::Inside, Form::Offset(_) | Form::Reference(_)) => {
                    deltas.push((at, payload));
                    at.1 = record.base as usize;
                }
                (Delta::Outside, Form::Reference(base)) => {
                    deltas.push((at, payload));
                    at = self.set.place(&base).ok_or(Error::MissingBase(base))?;
                }
                _ => return Err(Error::Corrupt("record and entry disagree")),
            }
            if deltas.len() > 4096 {
                return Err(Error::Corrupt("delta chain"));
            }
        };
        for (at, delta) in deltas.into_iter().rev() {
            let made = alt_git_pack::delta::apply(&data, &delta);
            data = Rc::new(made.map_err(|_| Error::Corrupt("delta"))?);
            self.keep(at, data.clone());
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &'static str, oid: u8, tree: bool) -> Entry<'static> {
        Entry {
            name: name.as_bytes(),
            oid: [oid; 20],
            tree,
        }
    }

    #[test]
    fn the_entries_of_two_trees_meet_in_the_order_git_writes_them() {
        // a directory is where its name with a slash after it would be
        let old = [
            entry("a-x", 1, false),
            entry("a.d", 2, false),
            entry("a", 3, true),
            entry("b", 4, false),
        ];
        let mut beside = Beside::new(&old);
        let found: Vec<_> = old.iter().map(|entry| beside.of(entry)).collect();
        assert_eq!(found, [1, 2, 3, 4].map(|oid| Some([oid; 20])));

        // a file where the directory was is another thing, before its neighbours
        let new = [
            entry("a", 9, false),
            entry("a-x", 1, false),
            entry("a.d", 2, false),
            entry("b", 4, false),
        ];
        let mut beside = Beside::new(&old);
        let found: Vec<_> = new.iter().map(|entry| beside.of(entry)).collect();
        assert_eq!(found[0], None);
        assert_eq!(found[1..], [1, 2, 4].map(|oid| Some([oid; 20])));

        // a name that begins with the name of a directory does not pass it
        let old = [entry("a", 3, true)];
        let new = [entry("a-x", 1, false), entry("a", 3, true)];
        let mut beside = Beside::new(&old);
        let found: Vec<_> = new.iter().map(|entry| beside.of(entry)).collect();
        assert_eq!(found, [None, Some([3; 20])]);

        // names that are gone, and names that are new
        let old = [entry("b", 1, false), entry("d", 2, true)];
        let new = [
            entry("a", 5, false),
            entry("c", 6, true),
            entry("d", 2, true),
        ];
        let mut beside = Beside::new(&old);
        let found: Vec<_> = new.iter().map(|entry| beside.of(entry)).collect();
        assert_eq!(found, [None, None, Some([2; 20])]);
    }

    #[test]
    fn a_tree_is_read_entry_by_entry_and_refused_where_it_is_not_one() {
        let mut data = b"100644 file\0".to_vec();
        data.extend_from_slice(&[1; 20]);
        data.extend_from_slice(b"160000 link\0");
        data.extend_from_slice(&[2; 20]);
        data.extend_from_slice(b"40000 dir\0");
        data.extend_from_slice(&[3; 20]);
        let read = entries(&data).unwrap();
        let read: Vec<_> = read.iter().map(|e| (e.name, e.oid[0], e.tree)).collect();
        assert_eq!(read, [(&b"file"[..], 1, false), (&b"dir"[..], 3, true)]);
        for bad in [
            &data[..data.len() - 1],
            &b"100644 name"[..],
            &b"100644"[..],
            &b"10\x00644 name\0aaaaaaaaaaaaaaaaaaaa"[..],
        ] {
            assert!(matches!(entries(bad), Err(Error::Corrupt("tree entry"))));
        }
    }
}
