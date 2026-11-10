use std::collections::VecDeque;

use alt_git_codec::{ObjectId, ObjectKind};

use crate::{
    PackError,
    delta_encode::{Index, TargetFilter},
};

/// Limits for optional in-pack delta encoding. Sources are already-emitted
/// objects; emitted REF deltas never depend on an external/thin-pack base.
#[derive(Debug, Clone, Copy)]
pub struct DeltaOptions {
    /// Recent candidates retained per object kind (at most 64).
    pub window: usize,
    /// Maximum retained canonical bytes, block indexes and optional search
    /// scratch. Fixed window metadata and caller-owned input are separate.
    pub memory_bytes: usize,
    /// Larger objects use ordinary encoding and are not retained as bases.
    pub max_object_bytes: usize,
    /// Maximum emitted delta-chain depth (at most 32).
    pub max_depth: u8,
}

impl Default for DeltaOptions {
    fn default() -> Self {
        Self {
            window: 8,
            memory_bytes: 96 << 20,
            max_object_bytes: 64 << 20,
            max_depth: 8,
        }
    }
}

struct Base {
    oid: ObjectId,
    kind: ObjectKind,
    depth: u8,
    index: Index,
}

pub(crate) struct Choice {
    pub(crate) base: ObjectId,
    pub(crate) depth: u8,
    pub(crate) delta: Vec<u8>,
}

pub(crate) struct Window {
    options: DeltaOptions,
    bases: VecDeque<Base>,
    bytes: usize,
}

impl Window {
    pub(crate) fn new(options: DeltaOptions) -> Result<Self, PackError> {
        if options.window > 64
            || options.max_depth > 32
            || options.max_object_bytes >= u32::MAX as usize
        {
            return Err(PackError::Format("delta encoding limits"));
        }
        Ok(Self {
            options,
            bases: VecDeque::new(),
            bytes: 0,
        })
    }

    fn eligible(&self, bytes: usize) -> bool {
        self.options.window != 0
            && self.options.max_depth != 0
            && bytes >= 64
            && bytes <= self.options.max_object_bytes
            && Index::allocation(bytes) <= self.options.memory_bytes
    }

    pub(crate) fn choose(&self, oid: ObjectId, kind: ObjectKind, data: &[u8]) -> Option<Choice> {
        if !self.eligible(data.len()) {
            return None;
        }
        let mut best = None;
        // A delta must leave room for the REF base and beat a full raw entry
        // substantially. This is a representation heuristic, not a guarantee
        // of optimal compressed size. Compression uses the existing backend.
        let mut limit = (data.len() / 2).saturating_sub(32);
        let mut work = (data.len() as u64).saturating_mul(8);
        let eligible = |base: &&Base| {
            base.kind == kind
                && base.oid != oid
                && base.depth < self.options.max_depth
                && base.index.data.len() / 2 <= data.len()
                && data.len() / 2 <= base.index.data.len()
        };
        // Scan the target once only when at least four candidates might
        // repeat the search. Large targets or a full memory budget retain the
        // ordinary path. The filter never excludes a possible anchor match.
        let filter = (self.bases.iter().filter(eligible).take(4).count() == 4)
            .then(|| TargetFilter::new(data, self.options.memory_bytes - self.bytes))
            .flatten();
        let mut filter_slots = data.len().saturating_mul(2);
        for base in self.bases.iter().rev().filter(eligible) {
            if filter
                .as_ref()
                .is_some_and(|filter| !base.index.may_match(filter, &mut filter_slots))
            {
                continue;
            }
            if let Some(delta) = base.index.encode(data, limit, &mut work) {
                limit = delta.len().saturating_sub(1);
                best = Some(Choice {
                    base: base.oid,
                    depth: base.depth + 1,
                    delta,
                });
            }
            if work == 0 {
                break;
            }
        }
        best
    }

    pub(crate) fn remember(&mut self, oid: ObjectId, kind: ObjectKind, data: &[u8], depth: u8) {
        if !self.eligible(data.len()) {
            return;
        }
        let allocation = Index::allocation(data.len());
        if self.bases.iter().filter(|base| base.kind == kind).count() >= self.options.window {
            let first = self
                .bases
                .iter()
                .position(|base| base.kind == kind)
                .unwrap();
            self.remove(first);
        }
        // Evict before allocating the new copy and index, including when a
        // large object follows many small objects of other kinds.
        while self.bytes > self.options.memory_bytes - allocation {
            self.remove(0);
        }
        self.bases.push_back(Base {
            oid,
            kind,
            depth,
            index: Index::new(data),
        });
        self.bytes += allocation;
    }

    fn remove(&mut self, position: usize) {
        let base = self.bases.remove(position).unwrap();
        self.bytes -= Index::allocation(base.index.data.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::HashAlgo;

    #[test]
    fn retention_respects_global_memory_and_per_kind_window() {
        let options = DeltaOptions {
            window: 2,
            memory_bytes: 16384,
            ..DeltaOptions::default()
        };
        let mut window = Window::new(options).unwrap();
        for i in 0..48 {
            let kind = [
                ObjectKind::Blob,
                ObjectKind::Tree,
                ObjectKind::Commit,
                ObjectKind::Tag,
            ][i % 4];
            let data = vec![i as u8; 1024 + i * 64];
            let oid = ObjectId::hash_object(HashAlgo::Sha1, kind, &data);
            window.remember(oid, kind, &data, 0);
            assert!(window.bytes <= options.memory_bytes);
            assert_eq!(
                window.bytes,
                window
                    .bases
                    .iter()
                    .map(|base| Index::allocation(base.index.data.len()))
                    .sum::<usize>()
            );
            assert!(window.bases.iter().filter(|base| base.kind == kind).count() <= options.window);
        }
    }
}
