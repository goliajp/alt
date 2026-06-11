use std::collections::HashMap;
use std::sync::Arc;

use alt_git_codec::ObjectKind;

/// Delta-base cache, keyed by pack offset.
///
/// Delta chains in real packs share long prefixes; without a cache, reading
/// N objects re-resolves the same bases O(N·depth) times. Budgeted like
/// git's `core.deltaBaseCacheLimit` (96 MiB default). First version: exact
/// LRU via a tick counter with linear-scan eviction — revisit in S6 bench.
pub(crate) struct DeltaBaseCache {
    map: HashMap<u64, Entry>,
    bytes: usize,
    budget: usize,
    tick: u64,
}

struct Entry {
    kind: ObjectKind,
    data: Arc<Vec<u8>>,
    last: u64,
}

pub(crate) const DEFAULT_BUDGET: usize = 96 * 1024 * 1024;

impl DeltaBaseCache {
    pub fn new(budget: usize) -> Self {
        Self {
            map: HashMap::new(),
            bytes: 0,
            budget,
            tick: 0,
        }
    }

    pub fn get(&mut self, offset: u64) -> Option<(ObjectKind, Arc<Vec<u8>>)> {
        self.tick += 1;
        let entry = self.map.get_mut(&offset)?;
        entry.last = self.tick;
        Some((entry.kind, entry.data.clone()))
    }

    pub fn put(&mut self, offset: u64, kind: ObjectKind, data: Arc<Vec<u8>>) {
        // an object bigger than a quarter of the budget would evict
        // everything else for one entry — not worth caching
        if data.len() > self.budget / 4 {
            return;
        }
        self.tick += 1;
        if let Some(old) = self.map.insert(
            offset,
            Entry {
                kind,
                data: data.clone(),
                last: self.tick,
            },
        ) {
            self.bytes -= old.data.len();
        }
        self.bytes += data.len();
        while self.bytes > self.budget {
            let (&victim, _) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.last)
                .expect("bytes > 0 implies entries exist");
            let evicted = self.map.remove(&victim).unwrap();
            self.bytes -= evicted.data.len();
        }
    }
}
