//! The index of a pack, as bytes.

use alt_git_codec::{HashAlgo, ObjectId};

/// V2 idx layout — same shape as [`crate::write::build_idx`], duplicated
/// here so we don't have to expose the writer's internals across modules
/// (and so this stays a self-contained read-and-index path).
pub(super) fn build_idx(
    algo: HashAlgo,
    entries: &mut [(ObjectId, u64, u32)],
    trailer: &[u8],
) -> Vec<u8> {
    use alt_hash::{Sha1, Sha256};
    entries.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut out = Vec::with_capacity(8 + 256 * 4 + entries.len() * (algo.raw_len() + 8) + 64);
    out.extend_from_slice(&[0xff, b't', b'O', b'c']);
    out.extend_from_slice(&2u32.to_be_bytes());

    let mut fanout = [0u32; 256];
    for (oid, _, _) in entries.iter() {
        fanout[oid.as_bytes()[0] as usize] += 1;
    }
    let mut cumulative = 0u32;
    for count in fanout {
        cumulative += count;
        out.extend_from_slice(&cumulative.to_be_bytes());
    }
    for (oid, _, _) in entries.iter() {
        out.extend_from_slice(oid.as_bytes());
    }
    for (_, _, crc) in entries.iter() {
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut large: Vec<u64> = Vec::new();
    for (_, offset, _) in entries.iter() {
        if *offset < (1 << 31) {
            out.extend_from_slice(&(*offset as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&((1u32 << 31) | large.len() as u32).to_be_bytes());
            large.push(*offset);
        }
    }
    for offset in large {
        out.extend_from_slice(&offset.to_be_bytes());
    }
    out.extend_from_slice(trailer);

    let checksum: Vec<u8> = match algo {
        HashAlgo::Sha1 => {
            let mut h = Sha1::new();
            h.update(&out);
            h.finalize().to_vec()
        }
        HashAlgo::Sha256 => {
            let mut h = Sha256::new();
            h.update(&out);
            h.finalize().to_vec()
        }
    };
    out.extend_from_slice(&checksum);
    out
}
