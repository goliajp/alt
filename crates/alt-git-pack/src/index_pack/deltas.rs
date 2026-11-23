//! The deltas of a pack, to be found by what they are made of.

/// What an entry of a pack is made of.
pub enum Of<Id> {
    /// a plain entry: of nothing
    Nothing,
    /// a delta of the entry at this index, which lies before it
    Entry(u32),
    /// a delta of the object with this id
    Object(Id),
}

/// The deltas of a pack in the order of what they are made of.
pub struct Deltas<Id> {
    /// `(base, delta)` by entry, in the order of the bases
    pub(super) by_offset: Vec<(u32, u32)>,
    /// `(base, delta)`, in the order of the ids of the bases
    pub(super) by_id: Vec<(Id, u32)>,
}

impl<Id: Ord + Copy> Deltas<Id> {
    /// `entries` in the order the pack has them.
    pub fn new(entries: impl IntoIterator<Item = Of<Id>>) -> Self {
        let (mut by_offset, mut by_id) = (Vec::new(), Vec::new());
        let mut count = 0u32;
        for of in entries {
            match of {
                Of::Nothing => {}
                Of::Entry(base) => by_offset.push((base, count)),
                Of::Object(base) => by_id.push((base, count)),
            }
            count += 1;
        }
        // how many entries rest on each by offset, itself included: a base
        // lies before its deltas
        let mut under = vec![1u32; count as usize];
        for (base, delta) in by_offset.iter().rev() {
            under[*base as usize] = under[*base as usize].saturating_add(under[*delta as usize]);
        }
        by_offset.sort_unstable_by_key(|(base, delta)| (*base, under[*delta as usize], *delta));
        by_id.sort_unstable();
        Deltas { by_offset, by_id }
    }

    /// Whether any delta is made of the entry `index`, the object `id`.
    pub fn any(&self, index: u32, id: &Id) -> bool {
        let (ofs, refs) = self.of(index, id);
        !ofs.is_empty() || !refs.is_empty()
    }

    /// The deltas named by id, those of one id together: the id and the entries.
    pub fn by_object(&self) -> impl Iterator<Item = &[(Id, u32)]> {
        self.by_id.chunk_by(|one, next| one.0 == next.0)
    }

    pub(super) fn of(
        &self,
        index: u32,
        id: &Id,
    ) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let ofs = self.by_offset.partition_point(|(base, _)| *base < index)
            ..self.by_offset.partition_point(|(base, _)| *base <= index);
        let refs = self.by_id.partition_point(|(base, _)| base < id)
            ..self.by_id.partition_point(|(base, _)| base <= id);
        (ofs, refs)
    }
}
