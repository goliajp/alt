//! The ref map in memory: the snapshot file as it sits on disk, read one
//! block of refs at a time as lookups reach them, under the refs changed
//! since. Opening costs the snapshot's checksum and nothing per ref.

use std::collections::BTreeMap;
use std::iter::Peekable;
use std::sync::OnceLock;

use alt_os::map::Mmap;

use crate::snapshot::parse_entry;
use crate::tx::RefChange;
use crate::{RefError, RefTarget};

/// Refs per block of a snapshot file.
pub(crate) const BLOCK: usize = 64;

/// One block of a snapshot, parsed.
struct Block {
    /// offset and length of each name in the file
    names: Vec<(u32, u16)>,
    targets: Vec<RefTarget>,
}

/// The refs of a snapshot file, parsed block by block on first use.
pub(crate) struct Base {
    map: Mmap,
    /// offset of the block table in `map`
    table: usize,
    count: usize,
    /// where the refs end in `map`
    end: usize,
    blocks: Vec<OnceLock<Block>>,
}

impl Base {
    /// `map` is a checksum-verified snapshot whose block table at `table`
    /// lies inside `map[..end]`.
    pub fn new(map: Mmap, table: usize, count: usize, end: usize) -> Self {
        let blocks = (0..count.div_ceil(BLOCK))
            .map(|_| OnceLock::new())
            .collect();
        Self {
            map,
            table,
            count,
            end,
            blocks,
        }
    }

    fn start(&self, block: usize) -> usize {
        if block == self.blocks.len() {
            return self.end;
        }
        let at = self.table + block * 4;
        u32::from_le_bytes(self.map[at..at + 4].try_into().unwrap()) as usize
    }

    /// The first name of a block, read without parsing the block.
    fn first_name(&self, block: usize) -> &[u8] {
        let at = self.start(block);
        let name = self.map.get(at..at + 2).and_then(|len| {
            let len = u16::from_le_bytes(len.try_into().unwrap()) as usize;
            self.map.get(at + 2..at + 2 + len)
        });
        name.unwrap_or_default()
    }

    fn block(&self, block: usize) -> &Block {
        self.blocks[block].get_or_init(|| {
            let data = &self.map[..self.start(block + 1).min(self.end)];
            let mut at = self.start(block).min(data.len());
            let mut names = Vec::with_capacity(BLOCK);
            let mut targets = Vec::with_capacity(BLOCK);
            // a snapshot that passed its checksum holds whole entries
            while names.len() < BLOCK && at < data.len() {
                let Some((name, target)) = parse_entry(data, &mut at) else {
                    break;
                };
                names.push((
                    (at - target_len(&target) - name.len()) as u32,
                    name.len() as u16,
                ));
                targets.push(target);
            }
            Block { names, targets }
        })
    }

    fn name(&self, block: &Block, at: usize) -> &str {
        let (start, len) = block.names[at];
        let bytes = &self.map[start as usize..start as usize + len as usize];
        // SAFETY: `parse_entry` validated this range as UTF-8 when the
        // block was parsed, and the mapping is never written
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    fn get(&self, name: &str) -> Option<&RefTarget> {
        // the last block whose first name is not above `name`
        let (mut lo, mut hi) = (0, self.blocks.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.first_name(mid) <= name.as_bytes() {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let block = self.block(lo.checked_sub(1)?);
        let at = block
            .names
            .binary_search_by(|&(start, len)| {
                self.map[start as usize..start as usize + len as usize].cmp(name.as_bytes())
            })
            .ok()?;
        Some(&block.targets[at])
    }

    fn iter(&self) -> impl Iterator<Item = (&str, &RefTarget)> {
        (0..self.blocks.len()).flat_map(move |i| {
            let block = self.block(i);
            (0..block.names.len()).map(move |at| (self.name(block, at), &block.targets[at]))
        })
    }
}

/// Encoded size of a target, as `parse_entry` consumed it.
fn target_len(target: &RefTarget) -> usize {
    match target {
        RefTarget::Oid(oid) => 2 + oid.as_bytes().len(),
        RefTarget::Symbolic(sym) => 3 + sym.len(),
    }
}

/// Every ref as it stands: the snapshot `base` overlaid with `changed`.
#[derive(Default)]
pub(crate) struct Refs {
    base: Option<Base>,
    /// refs set or (`None`) deleted since `base`
    changed: BTreeMap<String, Option<RefTarget>>,
}

impl Refs {
    pub fn from_base(base: Base) -> Self {
        Self {
            base: Some(base),
            changed: BTreeMap::new(),
        }
    }

    pub fn from_changes(changed: BTreeMap<String, Option<RefTarget>>) -> Self {
        Self {
            base: None,
            changed,
        }
    }

    pub fn get(&self, name: &str) -> Option<&RefTarget> {
        match self.changed.get(name) {
            Some(target) => target.as_ref(),
            None => self.base.as_ref()?.get(name),
        }
    }

    /// All refs in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &RefTarget)> {
        Merged {
            base: self.base.iter().flat_map(Base::iter).peekable(),
            changed: self.changed.iter().peekable(),
        }
    }

    pub fn len(&self) -> usize {
        let in_base = |name: &str| {
            self.base
                .as_ref()
                .is_some_and(|base| base.get(name).is_some())
        };
        let mut len = self.base.as_ref().map_or(0, |base| base.count);
        for (name, target) in &self.changed {
            match (in_base(name), target.is_some()) {
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
