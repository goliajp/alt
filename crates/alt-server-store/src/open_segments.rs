//! The mappings of segment files shared by every request of this process.

use alt_pack_segment::Segment;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};

/// A process may hold a limited number of mappings (65530 by default on Linux), and a
/// server that has touched more segments than that must not fail to map the next one.
const LIMIT: usize = 4096;

struct Recent<T> {
    entries: HashMap<PathBuf, (T, u64)>,
    clock: u64,
    limit: usize,
}

impl<T: Clone> Recent<T> {
    fn new(limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            limit,
        }
    }

    fn get(&mut self, path: &Path) -> Option<T> {
        self.clock += 1;
        let now = self.clock;
        self.entries.get_mut(path).map(|(value, used)| {
            *used = now;
            value.clone()
        })
    }

    fn insert(&mut self, path: PathBuf, value: T) {
        self.clock += 1;
        self.entries.insert(path, (value, self.clock));
        if self.entries.len() > self.limit {
            // the least recently used quarter goes at once, so the scan is paid once
            // per limit / 4 insertions
            let mut used: Vec<u64> = self.entries.values().map(|(_, used)| *used).collect();
            let cut = used.len() - self.limit * 3 / 4;
            let threshold = *used.select_nth_unstable(cut).1;
            self.entries.retain(|_, (_, used)| *used >= threshold);
        }
    }
}

static OPEN: LazyLock<Mutex<Recent<Arc<Segment>>>> =
    LazyLock::new(|| Mutex::new(Recent::new(LIMIT)));

pub(crate) fn get(path: &Path) -> Option<Arc<Segment>> {
    OPEN.lock().unwrap().get(path)
}

/// segments are immutable, so one mapping per file serves every request; a request
/// that still reads a segment dropped from here keeps its own handle
pub(crate) fn insert(path: PathBuf, segment: Arc<Segment>) {
    OPEN.lock().unwrap().insert(path, segment);
}

/// drops the shared mapping of a segment whose file is about to be deleted
pub(crate) fn forget(path: &Path) {
    OPEN.lock().unwrap().entries.remove(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(index: usize) -> PathBuf {
        PathBuf::from(index.to_string())
    }

    #[test]
    fn keeps_the_most_recently_used_and_stays_within_the_limit() {
        let mut recent = Recent::new(8);
        for index in 0..8 {
            recent.insert(path(index), index);
        }
        assert_eq!(recent.get(&path(0)), Some(0));
        recent.insert(path(8), 8);
        assert_eq!(recent.entries.len(), 6);
        assert_eq!(recent.get(&path(0)), Some(0));
        assert_eq!(recent.get(&path(8)), Some(8));
        for dropped in 1..4 {
            assert_eq!(recent.get(&path(dropped)), None);
        }
        for index in 9..100 {
            recent.insert(path(index), index);
            assert!(recent.entries.len() <= 8);
            assert_eq!(recent.get(&path(index)), Some(index));
        }
    }
}
