//! The capture slots of one path, kept as the C library keeps its
//! registers while it walks the path of a match.

/// Records position `at` in capture slot `slot` of a path. `slots` holds
/// the `width` capture slots in use and, where the groups of a program
/// with snapshots are asked for, as many again: what the first were
/// when a group last closed around something. A group that closes around nothing, in a turn of a
/// repetition that could have been left out and behind an earlier match
/// of the group, puts every slot back to that. Each slot that changes is
/// first given to `undo` with what it held, so that a path given up
/// costs what it wrote and not what it carries.
pub fn close(
    slots: &mut [usize],
    width: usize,
    slot: usize,
    at: usize,
    opt: bool,
    mut undo: impl FnMut(usize, usize),
) {
    let mut write = |slots: &mut [usize], slot: usize, value: usize| {
        if slots[slot] != value {
            undo(slot, slots[slot]);
            slots[slot] = value;
        }
    };
    if slots.len() == width || slot.is_multiple_of(2) {
        return write(slots, slot, at);
    }
    if slots[slot - 1] < at {
        write(slots, slot, at);
        for slot in 0..width {
            let now = slots[slot];
            write(slots, width + slot, now);
        }
    } else if opt && slots[width + slot - 1] != usize::MAX {
        for slot in 0..width {
            let kept = slots[width + slot];
            write(slots, slot, kept);
        }
    } else {
        write(slots, slot, at);
    }
}

/// Makes room in `items` for `more`, never for more than `most` in all
/// when that is enough: a vector left to itself takes up to twice what
/// it holds.
pub fn room<T>(items: &mut Vec<T>, more: usize, most: usize) {
    let needed = items.len() + more;
    if needed > items.capacity() {
        let wanted = needed.max(2 * items.capacity()).min(most.max(needed));
        items.reserve_exact(wanted - items.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_is_made_in_steps_up_to_the_most_and_no_further() {
        let mut items: Vec<u64> = Vec::new();
        for _ in 0..100 {
            room(&mut items, 1, 70);
            items.push(0);
            assert!(items.capacity() <= 70 || items.capacity() == items.len());
        }
        assert_eq!(items.capacity(), 100);
    }
}
