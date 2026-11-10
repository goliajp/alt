//! Bounded derived encodings shared by HTTP workers. Entries provide no read
//! authority and never substitute for the current pinned original bytes.
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};

use alt_git_codec::ObjectId;
use alt_git_pack::{PlainRepresentation, RepresentationLimits};
use alt_remote_protocol::Scope;

pub(crate) const RETENTION: RepresentationLimits = RepresentationLimits {
    bytes: 16 * 1024 * 1024,
    objects: 8192,
    object_bytes: 1024 * 1024,
};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    namespace: [u8; 32],
    oid: ObjectId,
    original: [u8; 32],
}

#[derive(Default)]
struct Entries {
    values: BTreeMap<Key, PlainRepresentation>,
    order: VecDeque<Key>,
    bytes: usize,
}

pub(crate) struct RepresentationCache {
    entries: Mutex<Entries>,
    limits: RepresentationLimits,
}

impl Default for RepresentationCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            limits: RETENTION,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Context<'a> {
    pub cache: &'a RepresentationCache,
    pub generation: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct Scoped<'a> {
    cache: &'a RepresentationCache,
    namespace: [u8; 32],
}

impl<'a> Context<'a> {
    pub(crate) fn scope(self, scope: &Scope) -> Scoped<'a> {
        let mut hash = blake3::Hasher::new();
        hash.update(b"alt.git.plain-representation.v1\0");
        for id in [&scope.installation_id, &scope.repository_instance_id] {
            hash.update(&(id.len() as u64).to_le_bytes());
            hash.update(id.as_bytes());
        }
        hash.update(&scope.recovery_epoch.to_le_bytes());
        hash.update(&self.generation.to_le_bytes());
        Scoped {
            cache: self.cache,
            namespace: *hash.finalize().as_bytes(),
        }
    }
}

impl Scoped<'_> {
    fn key(&self, oid: ObjectId, original: &[u8]) -> Key {
        Key {
            namespace: self.namespace,
            oid,
            original: *blake3::hash(original).as_bytes(),
        }
    }

    pub(crate) fn get(&self, oid: ObjectId, original: &[u8]) -> Option<PlainRepresentation> {
        if original.len() > self.cache.limits.object_bytes {
            return None;
        }
        let key = self.key(oid, original);
        self.cache.entries.lock().ok()?.values.get(&key).cloned()
    }

    pub(crate) fn remember(&self, original: &[u8], representation: PlainRepresentation) {
        let size = representation.encoded_bytes();
        let limits = self.cache.limits;
        if limits.objects == 0
            || size > limits.bytes
            || size > limits.object_bytes
            || original.len() != representation.object_bytes()
            || original.len() > limits.object_bytes
        {
            return;
        }
        let key = self.key(representation.oid(), original);
        let Ok(mut entries) = self.cache.entries.lock() else {
            return;
        };
        if entries.values.contains_key(&key) {
            return;
        }
        // FIFO eviction keeps both payloads and metadata bounded; lookups do
        // not append an unbounded history of repeated access records.
        while entries.values.len() >= limits.objects || size > limits.bytes - entries.bytes {
            let Some(oldest) = entries.order.pop_front() else {
                return;
            };
            if let Some(old) = entries.values.remove(&oldest) {
                entries.bytes -= old.encoded_bytes();
            }
        }
        entries.bytes += size;
        entries.order.push_back(key.clone());
        entries.values.insert(key, representation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::{HashAlgo, ObjectKind};
    use alt_git_pack::{IngressLimits, NoBases, PackWriter, ingest_pack_with_representations};

    fn fixture() -> Vec<(Vec<u8>, PlainRepresentation)> {
        let directory = tempfile::tempdir().unwrap();
        let mut writer = PackWriter::create(directory.path(), HashAlgo::Sha1, 12).unwrap();
        let data: Vec<Vec<u8>> = (0..12).map(|i| vec![i; 256]).collect();
        for original in &data {
            writer
                .add(
                    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, original),
                    ObjectKind::Blob,
                    original,
                )
                .unwrap();
        }
        let pack = writer.finish().unwrap();
        let decoded = ingest_pack_with_representations(
            &mut std::fs::File::open(pack.pack_path).unwrap(),
            HashAlgo::Sha1,
            IngressLimits {
                wire_bytes: 100000,
                objects: 100,
                object_bytes: 1000,
                inflated_bytes: 100000,
                resolved_bytes: 100000,
                delta_depth: 8,
                steps: 1000,
            },
            &mut NoBases,
            RETENTION,
        )
        .unwrap();
        data.into_iter().zip(decoded.representations).collect()
    }

    fn scope() -> Scope {
        Scope {
            installation_id: "installation".into(),
            repository_instance_id: "repository".into(),
            recovery_epoch: 1,
        }
    }

    #[test]
    fn cached_encodings_are_scoped_and_require_exact_current_content() {
        let cache = RepresentationCache::default();
        let context = Context {
            cache: &cache,
            generation: 1,
        };
        let scope = scope();
        let selected = context.scope(&scope);
        let (data, representation) = fixture().remove(0);
        let oid = representation.oid();
        assert!(selected.get(oid, &data).is_none());
        selected.remember(&data, representation);
        assert!(selected.get(oid, &data).is_some());
        assert!(selected.get(oid, &vec![1; data.len()]).is_none());
        for changed in [
            Scope {
                installation_id: "another".into(),
                ..scope.clone()
            },
            Scope {
                repository_instance_id: "another".into(),
                ..scope.clone()
            },
            Scope {
                recovery_epoch: 2,
                ..scope.clone()
            },
        ] {
            assert!(context.scope(&changed).get(oid, &data).is_none());
        }
        assert!(
            Context {
                generation: 2,
                ..context
            }
            .scope(&scope)
            .get(oid, &data)
            .is_none()
        );
        assert!(
            Context {
                cache: &RepresentationCache::default(),
                generation: 1
            }
            .scope(&scope)
            .get(oid, &data)
            .is_none()
        );
    }

    #[test]
    fn concurrent_insertions_and_repeated_hits_keep_both_bounds() {
        let objects = fixture();
        for budget in [20, 80] {
            let cache = RepresentationCache {
                entries: Mutex::new(Entries::default()),
                limits: RepresentationLimits {
                    bytes: budget,
                    objects: 3,
                    ..RETENTION
                },
            };
            std::thread::scope(|threads| {
                for generation in 1..=8 {
                    let cache = &cache;
                    let objects = &objects;
                    threads.spawn(move || {
                        let selected = Context { cache, generation }.scope(&scope());
                        for (data, representation) in objects {
                            selected.remember(data, representation.clone());
                            for _ in 0..20 {
                                let _ = selected.get(representation.oid(), data);
                            }
                        }
                    });
                }
            });
            let entries = cache.entries.lock().unwrap();
            assert!(entries.values.len() <= 3 && entries.bytes <= budget);
            assert_eq!(entries.values.len(), entries.order.len());
            assert_eq!(
                entries.bytes,
                entries
                    .values
                    .values()
                    .map(|r| r.encoded_bytes())
                    .sum::<usize>()
            );
            assert!(
                entries
                    .order
                    .iter()
                    .all(|key| entries.values.contains_key(key))
            );
        }
    }

    #[test]
    fn disabled_or_unavailable_cache_is_a_miss() {
        let cache = RepresentationCache {
            entries: Mutex::new(Entries::default()),
            limits: RepresentationLimits::default(),
        };
        let (data, representation) = fixture().remove(0);
        let oid = representation.oid();
        let selected = Context {
            cache: &cache,
            generation: 1,
        }
        .scope(&scope());
        selected.remember(&data, representation.clone());
        assert!(selected.get(oid, &data).is_none());
        let cache = RepresentationCache::default();
        let selected = Context {
            cache: &cache,
            generation: 1,
        }
        .scope(&scope());
        selected.remember(&data, representation.clone());
        assert!(selected.get(oid, &data).is_some());
        assert!(
            std::panic::catch_unwind(|| {
                let _lock = cache.entries.lock().unwrap();
                panic!("cache failure");
            })
            .is_err()
        );
        selected.remember(&data, representation);
        assert!(selected.get(oid, &data).is_none());
    }
}
