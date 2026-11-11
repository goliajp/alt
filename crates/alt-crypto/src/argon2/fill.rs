//! The memory-filling loop: which earlier block each new block is mixed with.

use super::block::{self, Block};

pub(super) struct Layout {
    pub lanes: usize,
    pub lane_len: usize,
    pub segment: usize,
    pub passes: u32,
}

/// fills one segment of one lane
pub(super) fn segment(memory: &mut [Block], layout: &Layout, pass: u32, slice: usize, lane: usize) {
    let compress = block::compress;
    // argon2id addresses the first half of the first pass independently of the data
    let independent = pass == 0 && slice < 2;
    let mut input = [0u64; 128];
    let mut addresses = [0u64; 128];
    if independent {
        input[..6].copy_from_slice(&[
            u64::from(pass),
            lane as u64,
            slice as u64,
            (layout.lanes * layout.lane_len) as u64,
            u64::from(layout.passes),
            2,
        ]);
    }
    // the first two blocks of every lane come from the seed
    let first = if pass == 0 && slice == 0 { 2 } else { 0 };
    if independent && first != 0 {
        next_addresses(compress, &mut input, &mut addresses);
    }
    let base = lane * layout.lane_len;
    let offset = slice * layout.segment;
    for index in first..layout.segment {
        let current = base + offset + index;
        let previous = if offset + index == 0 {
            current + layout.lane_len - 1
        } else {
            current - 1
        };
        let random = if independent {
            if index % 128 == 0 {
                next_addresses(compress, &mut input, &mut addresses);
            }
            addresses[index % 128]
        } else {
            memory[previous][0]
        };
        let same_lane_only = pass == 0 && slice == 0;
        let reference_lane = if same_lane_only {
            lane
        } else {
            (random >> 32) as usize % layout.lanes
        };
        let same_lane = reference_lane == lane;
        // blocks finished so far that this one may refer to
        let area = if pass == 0 {
            if same_lane {
                offset + index - 1
            } else {
                offset - usize::from(index == 0)
            }
        } else if same_lane {
            layout.lane_len - layout.segment + index - 1
        } else {
            layout.lane_len - layout.segment - usize::from(index == 0)
        } as u64;
        let j1 = random & 0xffff_ffff;
        let skew = (j1 * j1) >> 32;
        let back = area - 1 - ((area * skew) >> 32);
        let start = if pass == 0 || slice == 3 {
            0
        } else {
            offset + layout.segment
        };
        let reference =
            reference_lane * layout.lane_len + (start + back as usize) % layout.lane_len;

        let [previous, reference, target] = memory
            .get_disjoint_mut([previous, reference, current])
            .expect("a block is never mixed with itself or its predecessor's reference");
        compress(previous, reference, target, pass != 0);
    }
}

/// the next 128 data-independent addresses: G(0, G(0, input)) with the counter advanced
fn next_addresses(compress: block::Compress, input: &mut Block, addresses: &mut Block) {
    static ZERO: Block = [0; 128];
    input[6] += 1;
    let mut once = [0u64; 128];
    compress(&ZERO, input, &mut once, false);
    compress(&ZERO, &once, addresses, false);
}
