use crate::*;

const MAGIC: &[u8; 8] = b"ALTGIX01";
const OVERHEAD: usize = 48;

/// A secondary mapping to canonical identity, never directly to mutable storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitIndexEntry {
    pub oid: ObjectId,
    pub kind: GitKind,
    pub digest: Digest,
}
impl GitIndexEntry {
    fn size(self) -> usize {
        34 + self.oid.bytes().len()
    }
    fn validate(self) -> Result<()> {
        if self.oid.bytes().iter().all(|v| *v == 0) {
            return Err(Error::Format("zero Git index key"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitIndexBlock {
    id: Digest,
    entries: Vec<GitIndexEntry>,
    encoded: Vec<u8>,
}
impl GitIndexBlock {
    pub fn build(mut entries: Vec<GitIndexEntry>, limits: Limits) -> Result<Self> {
        limits.validate()?;
        if entries.len() > limits.members {
            return Err(Error::Limit("Git index input members"));
        }
        entries.sort_by_key(|e| e.oid);
        entries.dedup();
        Self::from_sorted(entries, limits)
    }
    fn from_sorted(entries: Vec<GitIndexEntry>, limits: Limits) -> Result<Self> {
        validate(&entries)?;
        let size = OVERHEAD + entries.iter().map(|e| e.size()).sum::<usize>();
        if entries.len() > limits.members || size > limits.bytes {
            return Err(Error::Limit("Git index extent"));
        }
        let mut encoded = Vec::with_capacity(size);
        encoded.extend(MAGIC);
        encoded.extend((entries.len() as u32).to_le_bytes());
        encoded.extend(((size - OVERHEAD) as u32).to_le_bytes());
        for e in &entries {
            encoded.push(e.oid.algorithm());
            encoded.extend(e.oid.bytes());
            encoded.push(e.kind as u8);
            encoded.extend(e.digest);
        }
        let id = *blake3::hash(&encoded).as_bytes();
        encoded.extend(id);
        Ok(Self {
            id,
            entries,
            encoded,
        })
    }
    pub fn decode(bytes: &[u8], expected: &Digest, limits: Limits) -> Result<Self> {
        limits.validate()?;
        if bytes.len() < OVERHEAD || bytes.len() > limits.bytes {
            return Err(Error::Limit("Git index bytes"));
        }
        let end = bytes.len() - 32;
        if &bytes[end..] != expected || blake3::hash(&bytes[..end]).as_bytes() != expected {
            return Err(Error::Format("Git index identity"));
        }
        let mut cursor = codec::Cursor(&bytes[..end]);
        if &cursor.take::<8>()? != MAGIC {
            return Err(Error::Format("Git index version"));
        }
        let count = u32::from_le_bytes(cursor.take()?) as usize;
        let size = u32::from_le_bytes(cursor.take()?) as usize;
        if count > limits.members || size != cursor.0.len() || count > size / 54 {
            return Err(Error::Format("Git index count or extent"));
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let oid = match cursor.byte()? {
                1 => ObjectId::Sha1(cursor.take()?),
                2 => ObjectId::Sha256(cursor.take()?),
                _ => return Err(Error::Format("Git index algorithm")),
            };
            let kind = match cursor.byte()? {
                0 => GitKind::Commit,
                1 => GitKind::Tree,
                2 => GitKind::Blob,
                3 => GitKind::Tag,
                _ => return Err(Error::Format("Git index kind")),
            };
            entries.push(GitIndexEntry {
                oid,
                kind,
                digest: cursor.take()?,
            });
        }
        if !cursor.0.is_empty() {
            return Err(Error::Format("Git index trailing bytes"));
        }
        validate(&entries)?;
        Ok(Self {
            id: *expected,
            entries,
            encoded: bytes.to_vec(),
        })
    }
    pub fn id(&self) -> &Digest {
        &self.id
    }
    pub fn entries(&self) -> &[GitIndexEntry] {
        &self.entries
    }
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }
    pub fn find(&self, oid: &ObjectId) -> Option<&GitIndexEntry> {
        self.entries
            .binary_search_by_key(oid, |e| e.oid)
            .ok()
            .map(|i| &self.entries[i])
    }
    pub fn page(&self) -> Result<Page<ObjectId>> {
        Page::new(
            self.id,
            self.entries.len(),
            self.entries.first().map(|e| e.oid),
            self.entries.last().map(|e| e.oid),
        )
    }
}

fn validate(entries: &[GitIndexEntry]) -> Result<()> {
    let mut previous = None;
    for e in entries {
        e.validate()?;
        if previous.is_some_and(|p| p >= e.oid) {
            return Err(Error::Conflict("Git index order or mapping"));
        }
        previous = Some(e.oid);
    }
    Ok(())
}

/// Bulk construction; the caller must validate equivalence with canonical pages.
/// At runtime both roots must change in the same repository transaction.
pub fn paginate_git(mut entries: Vec<GitIndexEntry>, limits: Limits) -> Result<Vec<GitIndexBlock>> {
    limits.validate()?;
    entries.sort_by_key(|e| e.oid);
    entries.dedup();
    validate(&entries)?;
    let mut output = Vec::new();
    let mut page = Vec::new();
    let mut size = OVERHEAD;
    for e in entries {
        if e.size() > limits.bytes - OVERHEAD {
            return Err(Error::Limit("Git index entry"));
        }
        if page.len() == limits.members || size + e.size() > limits.bytes {
            output.push(GitIndexBlock::from_sorted(
                std::mem::take(&mut page),
                limits,
            )?);
            size = OVERHEAD;
        }
        size += e.size();
        page.push(e);
    }
    if !page.is_empty() {
        output.push(GitIndexBlock::from_sorted(page, limits)?);
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug)]
pub enum GitIndexEdit {
    Insert(GitIndexEntry),
    Remove(ObjectId),
}

/// Bounded edits to one secondary page. Existing OID mappings cannot be
/// replaced: changing kind/canonical identity is a conflict. Cross-page and
/// retained-pin consistency still belongs to the repository transaction.
pub fn edit_git_page(
    old: &GitIndexBlock,
    expected: &Digest,
    range: &KeyRange<ObjectId>,
    edits: Vec<GitIndexEdit>,
    limits: Limits,
) -> Result<Vec<GitIndexBlock>> {
    limits.validate()?;
    range.validate()?;
    if old.id() != expected {
        return Err(Error::Conflict("stale Git index page"));
    }
    if old.entries.len() > limits.members
        || old.encoded.len() > limits.bytes
        || edits.len() > limits.members
    {
        return Err(Error::Limit("Git index edits or old page"));
    }
    if old.entries.iter().any(|e| !range.contains(&e.oid)) {
        return Err(Error::Conflict("old Git index interval"));
    }
    let mut seen = BTreeSet::new();
    let mut bytes = OVERHEAD;
    for edit in &edits {
        let key = match edit {
            GitIndexEdit::Insert(e) => e.oid,
            GitIndexEdit::Remove(oid) => *oid,
        };
        if !range.contains(&key) || !seen.insert(key) {
            return Err(Error::Conflict("Git index edit interval or duplicate"));
        }
        bytes += match edit {
            GitIndexEdit::Insert(e) => {
                e.validate()?;
                e.size()
            }
            GitIndexEdit::Remove(oid) => oid.key().len(),
        };
        if bytes > limits.bytes {
            return Err(Error::Limit("Git index edit bytes"));
        }
        if matches!(edit, GitIndexEdit::Insert(_)) == old.find(&key).is_some() {
            return Err(Error::Conflict("Git index edit presence"));
        }
    }
    let mut entries: BTreeMap<_, _> = old.entries.iter().copied().map(|e| (e.oid, e)).collect();
    for edit in edits {
        match edit {
            GitIndexEdit::Insert(e) => {
                entries.insert(e.oid, e);
            }
            GitIndexEdit::Remove(k) => {
                entries.remove(&k);
            }
        }
    }
    paginate_git(entries.into_values().collect(), limits)
}
