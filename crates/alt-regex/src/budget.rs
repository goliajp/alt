//! The bytes the tree and the program of one pattern may hold.

use crate::Error;

/// A pattern whose tree and program come to more bytes than this is
/// refused. What is held for a moment while they are made, a vector as
/// it grows or the places of one set of characters, comes on top: some
/// 32 MiB in all.
pub const MAX_BYTES: usize = 24 << 20;

/// The bytes counted so far.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Budget {
    held: usize,
}

impl Budget {
    pub fn take(&mut self, bytes: usize) -> Result<(), Error> {
        self.held += bytes;
        match self.held > MAX_BYTES {
            true => Err(Error::TooBig),
            false => Ok(()),
        }
    }

    pub fn give(&mut self, bytes: usize) {
        self.held -= bytes;
    }

    /// Adds `item` to `items` and counts the room the vector took.
    pub fn push<T>(&mut self, items: &mut Vec<T>, item: T) -> Result<(), Error> {
        let room = items.capacity();
        items.push(item);
        self.take((items.capacity() - room) * size_of::<T>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_room_of_a_vector_is_counted_as_it_grows() {
        let mut budget = Budget::default();
        let mut items = Vec::new();
        for item in 0..1000u64 {
            budget.push(&mut items, item).unwrap();
        }
        assert_eq!(budget.held, items.capacity() * 8);
        budget.give(items.capacity() * 8);
        assert_eq!(budget.take(MAX_BYTES), Ok(()));
        assert_eq!(budget.take(1), Err(Error::TooBig));
    }
}
