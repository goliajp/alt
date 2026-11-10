use crate::*;

/// A nonempty page descriptor. SQL range columns are only routing hints until
/// compared with a decoded block's descriptor. Root order and neighbor bounds
/// must be checked by the caller within the repository transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<K> {
    pub id: Digest,
    pub members: usize,
    pub first: K,
    pub last: K,
}
impl<K: Ord> Page<K> {
    pub(crate) fn new(
        id: Digest,
        members: usize,
        first: Option<K>,
        last: Option<K>,
    ) -> Result<Self> {
        let (Some(first), Some(last)) = (first, last) else {
            return Err(Error::Format("empty root page"));
        };
        let page = Self {
            id,
            members,
            first,
            last,
        };
        page.validate()?;
        Ok(page)
    }
    pub fn validate(&self) -> Result<()> {
        if self.members == 0
            || self.members > MAX_MEMBERS
            || self.first > self.last
            || (self.members == 1) != (self.first == self.last)
        {
            return Err(Error::Format("page descriptor"));
        }
        Ok(())
    }
    pub fn contains(&self, key: &K) -> bool {
        self.first <= *key && *key <= self.last
    }
    pub fn precedes(&self, next: &Self) -> Result<()> {
        self.validate()?;
        next.validate()?;
        if self.last >= next.first {
            return Err(Error::Conflict("overlapping root pages"));
        }
        Ok(())
    }
}

/// The key interval assigned to one old page, including gaps where inserts may
/// land. Supplied from current neighbors under the same root lock as the CAS.
#[derive(Clone, Debug)]
pub struct KeyRange<K> {
    pub lower: Option<K>,
    pub upper: Option<K>,
}
impl<K: Ord> KeyRange<K> {
    pub(crate) fn validate(&self) -> Result<()> {
        if let (Some(low), Some(high)) = (&self.lower, &self.upper)
            && low >= high
        {
            return Err(Error::Format("edit interval"));
        }
        Ok(())
    }
    pub(crate) fn contains(&self, key: &K) -> bool {
        self.lower.as_ref().is_none_or(|v| key >= v) && self.upper.as_ref().is_none_or(|v| key < v)
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Insert(Entry),
    Replace(Entry),
    Remove(Digest),
}
impl Block {
    pub fn page(&self) -> Result<Page<Digest>> {
        Page::new(
            self.id,
            self.entries.len(),
            self.entries.first().map(|e| e.digest),
            self.entries.last().map(|e| e.digest),
        )
    }
}

/// Rebuild only one existing page, possibly splitting it or removing it entirely.
/// Old page/pins are immutable. Input edits are bounded to one page worth of
/// members and bytes; output is bounded by the old page plus that input.
///
/// This validates page-local identities only. The caller must enforce repository
/// size/OID consistency (including retained pins), update the secondary index,
/// and CAS the root transactionally. No database or publication occurs here.
pub fn edit_page(
    old: &Block,
    expected: &Digest,
    range: &KeyRange<Digest>,
    edits: Vec<Edit>,
    limits: Limits,
) -> Result<Vec<Block>> {
    limits.validate()?;
    range.validate()?;
    if old.id() != expected {
        return Err(Error::Conflict("stale page"));
    }
    if old.entries.len() > limits.members
        || old.encoded.len() > limits.bytes
        || edits.len() > limits.members
    {
        return Err(Error::Limit("edit members or old page"));
    }
    if old.entries.iter().any(|e| !range.contains(&e.digest)) {
        return Err(Error::Conflict("old page interval"));
    }
    let mut seen = BTreeSet::new();
    let mut bytes = codec::OVERHEAD;
    // Validate before cloning or changing any old entry.
    for edit in &edits {
        let key = match edit {
            Edit::Insert(e) | Edit::Replace(e) => &e.digest,
            Edit::Remove(k) => k,
        };
        if !range.contains(key) || !seen.insert(*key) {
            return Err(Error::Conflict("edit interval or duplicate"));
        }
        let size = match edit {
            Edit::Insert(e) | Edit::Replace(e) => {
                e.validate()?;
                codec::entry_size(e)
            }
            Edit::Remove(_) => 32,
        };
        bytes = bytes.checked_add(size).ok_or(Error::Limit("edit bytes"))?;
        if bytes > limits.bytes {
            return Err(Error::Limit("edit bytes"));
        }
        let existing = old.find(key);
        if matches!(edit, Edit::Insert(_)) == existing.is_some() {
            return Err(Error::Conflict("edit presence"));
        }
        if let (Edit::Replace(next), Some((_, previous))) = (edit, existing)
            && (next.bytes != previous.bytes || previous.git.iter().any(|g| !next.git.contains(g)))
        {
            return Err(Error::Conflict("replacement changes content identity"));
        }
    }
    let mut entries: BTreeMap<_, _> = old.entries.iter().cloned().map(|e| (e.digest, e)).collect();
    for edit in edits {
        match edit {
            Edit::Insert(e) | Edit::Replace(e) => {
                entries.insert(e.digest, e);
            }
            Edit::Remove(k) => {
                entries.remove(&k);
            }
        }
    }
    paginate(entries.into_values().collect(), limits)
}
