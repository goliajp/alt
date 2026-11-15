//! The ref map in memory: the snapshot as it sits in its file (names are
//! read in place, found by binary search) under the refs changed since.
//! Opening costs one pass over the snapshot's bytes and nothing per ref
//! beyond that, however many refs a command then looks at.

use std::collections::BTreeMap;
use std::iter::Peekable;

use crate::tx::RefChange;
use crate::{RefError, RefTarget};

/// The refs of a snapshot file, kept in the file's encoding.
#[derive(Default)]
pub(crate) struct Base {
    data: Vec<u8>,
    /// offset and length of each name in `data`, names ascending
    names: Vec<(u32, u16)>,
    targets: Vec<RefTarget>,
}

impl Base {
    /// `names` must be valid UTF-8 ranges of `data` in ascending name order,
    /// one per target.
    pub fn new(data: Vec<u8>, names: Vec<(u32, u16)>, targets: Vec<RefTarget>) -> Self {
        Self {
            data,
            names,
            targets,
        }
    }

    fn name(&self, at: usize) -> &str {
        let (start, len) = self.names[at];
        let bytes = &self.data[start as usize..start as usize + len as usize];
        // SAFETY: the snapshot reader validated every name as UTF-8
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    fn find(&self, name: &str) -> Option<usize> {
        let (mut lo, mut hi) = (0, self.names.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (start, len) = self.names[mid];
            let found = &self.data[start as usize..start as usize + len as usize];
            match found.cmp(name.as_bytes()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    fn iter(&self) -> impl Iterator<Item = (&str, &RefTarget)> {
        (0..self.names.len()).map(|at| (self.name(at), &self.targets[at]))
    }
}

/// Every ref as it stands: `base` overlaid with `changed`.
#[derive(Default)]
pub(crate) struct Refs {
    base: Base,
    /// refs set or (`None`) deleted since `base`
    changed: BTreeMap<String, Option<RefTarget>>,
}

impl Refs {
    pub fn from_base(base: Base) -> Self {
        Self {
            base,
            changed: BTreeMap::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&RefTarget> {
        match self.changed.get(name) {
            Some(target) => target.as_ref(),
            None => self.base.find(name).map(|at| &self.base.targets[at]),
        }
    }

    /// All refs in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &RefTarget)> {
        Merged {
            base: self.base.iter().peekable(),
            changed: self.changed.iter().peekable(),
        }
    }

    pub fn len(&self) -> usize {
        let mut len = self.base.names.len();
        for (name, target) in &self.changed {
            match (self.base.find(name).is_some(), target.is_some()) {
                (false, true) => len += 1,
                (true, false) => len -= 1,
                _ => {}
            }
        }
        len
    }

    pub fn to_map(&self) -> BTreeMap<String, RefTarget> {
        self.iter()
            .map(|(name, target)| (name.to_owned(), target.clone()))
            .collect()
    }

    /// Enforces the transaction's expected-old values. `replaying` only
    /// changes the error: a mismatch during replay means the log and the
    /// derived state diverged (corruption), not a caller conflict.
    pub fn check(&self, changes: &[RefChange], replaying: bool) -> Result<(), RefError> {
        for change in changes {
            if self.get(&change.name) != change.old.as_ref() {
                return Err(if replaying {
                    RefError::Format("ref state diverged during replay")
                } else {
                    RefError::Conflict {
                        name: change.name.clone(),
                    }
                });
            }
        }
        Ok(())
    }

    /// Checks, then applies, one transaction.
    pub fn apply(&mut self, changes: &[RefChange], replaying: bool) -> Result<(), RefError> {
        self.check(changes, replaying)?;
        for change in changes {
            self.changed.insert(change.name.clone(), change.new.clone());
        }
        Ok(())
    }
}

type BaseItem<'a> = (&'a str, &'a RefTarget);
type ChangedItem<'a> = (&'a String, &'a Option<RefTarget>);

struct Merged<'a, B: Iterator<Item = BaseItem<'a>>, C: Iterator<Item = ChangedItem<'a>>> {
    base: Peekable<B>,
    changed: Peekable<C>,
}

impl<'a, B, C> Iterator for Merged<'a, B, C>
where
    B: Iterator<Item = BaseItem<'a>>,
    C: Iterator<Item = ChangedItem<'a>>,
{
    type Item = BaseItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let order = match (self.base.peek(), self.changed.peek()) {
                (None, None) => return None,
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some((base, _)), Some((changed, _))) => (*base).cmp(changed.as_str()),
            };
            if order == std::cmp::Ordering::Less {
                return self.base.next();
            }
            if order == std::cmp::Ordering::Equal {
                self.base.next();
            }
            let (name, target) = self.changed.next()?;
            if let Some(target) = target {
                return Some((name.as_str(), target));
            }
        }
    }
}
