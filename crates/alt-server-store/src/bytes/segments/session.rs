//! Parsed indexes belong to one protected, buffered operation. Every selected
//! payload still goes through its caller's canonical-content verification.
use super::*;
use std::{collections::BTreeMap, sync::Mutex};

const MAX_INDEX_FILES: usize = 32;
const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;

struct Entry {
    index: Index,
    stamp: Stamp,
    used: u64,
}
impl Entry {
    fn allocation(&self) -> usize {
        std::mem::size_of_val(self.index.members())
    }
}
#[derive(Default)]
struct Indexes {
    entries: BTreeMap<String, Entry>,
    bytes: usize,
    clock: u64,
}

/// No descriptor or permission is cached. A caller must finish this session
/// before its buffered result or verified publication can escape; eviction
/// performs the same full index check before forgetting an earlier selection.
pub(crate) struct ReadSession {
    store: ByteStore,
    scope: Scope,
    indexes: Mutex<Indexes>,
}
impl ReadSession {
    pub(crate) fn new(store: &ByteStore, scope: &Scope) -> Self {
        Self {
            store: store.clone(),
            scope: scope.clone(),
            indexes: Mutex::new(Indexes::default()),
        }
    }
    pub(crate) fn read(
        &self,
        digest: &str,
        bytes: u64,
        location: Option<&SegmentLocation>,
    ) -> Result<StoredReader> {
        let Some(location) = location else {
            return self.store.read_payload(&self.scope, digest, bytes, None);
        };
        check_digest(digest)?;
        let digest = alt_hash::blake3::Hash::from_hex(digest)
            .map_err(|_| Error::Integrity("segment digest"))?;
        let mut file = open_segment(&self.store.segment_path(&self.scope, &location.segment_id)?)?;
        let stamp = Stamp::read(&file)?;
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| Error::Integrity("segment read session poisoned"))?;
        indexes.clock = indexes.clock.saturating_add(1);
        let clock = indexes.clock;
        let offset = if let Some(entry) = indexes.entries.get_mut(&location.segment_id) {
            if entry.stamp != stamp {
                return Err(Error::Precondition("selected segment changed"));
            }
            entry.used = clock;
            select(&entry.index, digest.as_bytes(), bytes, location)?
        } else {
            let index = Segment::open(&mut file, Limits::default())
                .map_err(segment_error)?
                .index()
                .clone();
            if Stamp::read(&file)? != stamp {
                return Err(Error::Precondition("segment changed while opening index"));
            }
            let offset = select(&index, digest.as_bytes(), bytes, location)?;
            let entry = Entry {
                index,
                stamp,
                used: clock,
            };
            let allocation = entry.allocation();
            while indexes.entries.len() >= MAX_INDEX_FILES
                || indexes.bytes + allocation > MAX_INDEX_BYTES
            {
                let id = indexes
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(id, _)| id.clone())
                    .ok_or(Error::Limit("segment read index budget"))?;
                self.check(&id, &indexes.entries[&id])?;
                let old = indexes.entries.remove(&id).unwrap();
                indexes.bytes -= old.allocation();
            }
            indexes.bytes += allocation;
            indexes.entries.insert(location.segment_id.clone(), entry);
            offset
        };
        Ok(StoredReader {
            file: Arc::new(file),
            start: offset,
            bytes,
            position: 0,
        })
    }
    /// Hash and parse the current complete indexes again. Metadata alone is
    /// never their integrity proof, including same-timestamp mutations.
    pub(crate) fn finish(&self) -> Result<()> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| Error::Integrity("segment read session poisoned"))?;
        for (id, entry) in &indexes.entries {
            self.check(id, entry)?;
        }
        Ok(())
    }
    fn check(&self, id: &str, entry: &Entry) -> Result<()> {
        let file = open_segment(&self.store.segment_path(&self.scope, id)?)?;
        if Stamp::read(&file)? != entry.stamp {
            return Err(Error::Precondition("selected segment changed"));
        }
        let segment = Segment::open(file, Limits::default()).map_err(segment_error)?;
        if segment.index() != &entry.index || Stamp::read(segment.source())? != entry.stamp {
            return Err(Error::Integrity("selected segment index changed"));
        }
        Ok(())
    }
}

fn select(index: &Index, digest: &[u8; 32], bytes: u64, location: &SegmentLocation) -> Result<u64> {
    let member = index
        .find(digest)
        .ok_or(Error::Integrity("missing segment member"))?;
    if index.digest() != &location.index_digest
        || member.offset() != location.offset
        || member.bytes() != bytes
    {
        return Err(Error::Integrity("segment location differs"));
    }
    Ok(member.offset())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn scope() -> Scope {
        Scope {
            installation_id: "installation".into(),
            repository_instance_id: "repository".into(),
            recovery_epoch: 1,
        }
    }
    fn put(store: &ByteStore, id: usize) -> DurableSegment {
        let mut source = Cursor::new(b"original");
        store
            .put_segment(
                &scope(),
                &format!("{id:032x}"),
                &mut [Input {
                    digest: *alt_hash::blake3::hash(b"original").as_bytes(),
                    bytes: 8,
                    source: &mut source,
                }],
            )
            .unwrap()
    }
    fn read(session: &ReadSession, proof: &DurableSegment) -> Result<Vec<u8>> {
        let digest = alt_hash::blake3::hash(b"original");
        let mut reader = session.read(
            &digest.to_hex(),
            8,
            proof.location(digest.as_bytes()).as_ref(),
        )?;
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;
        // The session returns a bounded raw reader, just like ObjectSource.
        // A caller must verify canonical bytes before accepting a result.
        if alt_hash::blake3::hash(&data) != digest {
            return Err(Error::Integrity("test canonical content changed"));
        }
        Ok(data)
    }

    #[test]
    fn warm_index_does_not_hide_missing_replaced_or_modified_files() {
        for fault in 0..5 {
            let temp = tempfile::tempdir().unwrap();
            let store = ByteStore::open(temp.path()).unwrap();
            let proof = put(&store, 1);
            let session = ReadSession::new(&store, &scope());
            assert_eq!(read(&session, &proof).unwrap(), b"original");
            let path = store.segment_path(&scope(), proof.id()).unwrap();
            match fault {
                0 => fs::remove_file(&path).unwrap(),
                1 => fs::write(&path, b"short").unwrap(),
                2 | 3 => {
                    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
                    file.seek(SeekFrom::Start(if fault == 2 {
                        64
                    } else {
                        proof.index().members()[0].offset()
                    }))
                    .unwrap();
                    file.write_all(b"X").unwrap();
                }
                _ => {
                    // Keep the old inode alive to ensure this is a distinct file.
                    fs::rename(&path, path.with_extension("old")).unwrap();
                    fs::copy(path.with_extension("old"), &path).unwrap();
                }
            }
            let selected = read(&session, &proof);
            let finished = session.finish();
            // A same-tick index mutation may pass the per-open metadata check;
            // its mandatory final cryptographic check must still reject it.
            assert!(selected.is_err() || finished.is_err(), "fault {fault}");
            if fault != 3 {
                assert!(finished.is_err(), "index/path fault {fault}");
            }
            if matches!(fault, 0 | 1 | 4) {
                assert!(selected.is_err(), "physical identity fault {fault}");
            }
        }
    }

    #[test]
    fn finish_and_eviction_rehash_even_if_metadata_comparison_passes() {
        for eviction in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let store = ByteStore::open(temp.path()).unwrap();
            let session = ReadSession::new(&store, &scope());
            let first = put(&store, 1);
            assert_eq!(read(&session, &first).unwrap(), b"original");
            if eviction {
                for id in 2..=MAX_INDEX_FILES {
                    let proof = put(&store, id);
                    assert_eq!(read(&session, &proof).unwrap(), b"original");
                }
            }
            let path = store.segment_path(&scope(), first.id()).unwrap();
            let mut file = OpenOptions::new().write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(64)).unwrap();
            file.write_all(b"X").unwrap();
            // Model a metadata collision without relying on the host clock's
            // granularity. The independent cryptographic check must still fail.
            session
                .indexes
                .lock()
                .unwrap()
                .entries
                .get_mut(first.id())
                .unwrap()
                .stamp = Stamp::read(&file).unwrap();
            if eviction {
                let next = put(&store, MAX_INDEX_FILES + 1);
                assert!(read(&session, &next).is_err());
            } else {
                assert!(session.finish().is_err());
            }
        }
    }

    #[test]
    fn bounded_indexes_reopen_evicted_members_and_reject_changed_locations() {
        let temp = tempfile::tempdir().unwrap();
        let store = ByteStore::open(temp.path()).unwrap();
        let session = ReadSession::new(&store, &scope());
        let first = put(&store, 1);
        for id in 1..=MAX_INDEX_FILES + 4 {
            let proof = if id == 1 { None } else { Some(put(&store, id)) };
            assert_eq!(
                read(&session, proof.as_ref().unwrap_or(&first)).unwrap(),
                b"original"
            );
            let indexes = session.indexes.lock().unwrap();
            assert!(indexes.entries.len() <= MAX_INDEX_FILES);
            assert!(indexes.bytes <= MAX_INDEX_BYTES);
        }
        assert!(
            !session
                .indexes
                .lock()
                .unwrap()
                .entries
                .contains_key(first.id())
        );
        assert_eq!(read(&session, &first).unwrap(), b"original");
        let digest = alt_hash::blake3::hash(b"original");
        let mut location = first.location(digest.as_bytes()).unwrap();
        location.offset += 1;
        assert!(session.read(&digest.to_hex(), 8, Some(&location)).is_err());
        session.finish().unwrap();
        let mut other = scope();
        other.repository_instance_id = "another".into();
        assert!(read(&ReadSession::new(&store, &other), &first).is_err());
    }
    #[test]
    fn large_indexes_hit_byte_budget_before_file_budget() {
        let temp = tempfile::tempdir().unwrap();
        let store = ByteStore::open(temp.path()).unwrap();
        let session = ReadSession::new(&store, &scope());
        let values: Vec<_> = (0..alt_byte_segment::MAX_MEMBERS as u64)
            .map(|n| n.to_le_bytes())
            .collect();
        let digest = alt_hash::blake3::hash(&values[0]);
        let mut first = None;
        for id in 1..=24 {
            let mut sources: Vec<_> = values.iter().map(|v| Cursor::new(v.as_slice())).collect();
            let mut inputs: Vec<_> = values
                .iter()
                .zip(&mut sources)
                .map(|(v, source)| Input {
                    digest: *alt_hash::blake3::hash(v).as_bytes(),
                    bytes: 8,
                    source,
                })
                .collect();
            let proof = store
                .put_segment(&scope(), &format!("{id:032x}"), &mut inputs)
                .unwrap();
            let location = proof.location(digest.as_bytes()).unwrap();
            let mut reader = session.read(&digest.to_hex(), 8, Some(&location)).unwrap();
            let mut actual = Vec::new();
            reader.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, values[0]);
            if id == 1 {
                first = Some(location);
            }
            let indexes = session.indexes.lock().unwrap();
            assert!(indexes.bytes <= MAX_INDEX_BYTES);
        }
        assert!(session.indexes.lock().unwrap().entries.len() < 24);
        let first = first.unwrap();
        assert!(
            !session
                .indexes
                .lock()
                .unwrap()
                .entries
                .contains_key(&first.segment_id)
        );
        session.read(&digest.to_hex(), 8, Some(&first)).unwrap();
        session.finish().unwrap();
    }
}
