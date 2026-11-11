//! XXH64, the hash behind the frame content checksum.

const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P3: u64 = 0x1656_67B1_9E37_79F9;
const P4: u64 = 0x85EB_CA77_C2B2_AE63;
const P5: u64 = 0x27D4_EB2F_1656_67C5;

#[inline(always)]
fn round(acc: u64, lane: u64) -> u64 {
    acc.wrapping_add(lane.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

#[inline(always)]
fn merge(hash: u64, acc: u64) -> u64 {
    (hash ^ round(0, acc)).wrapping_mul(P1).wrapping_add(P4)
}

/// XXH64 of `data` with the given seed.
pub(crate) fn xxh64(data: &[u8], seed: u64) -> u64 {
    let (stripes, rest) = data.as_chunks::<32>();
    let mut hash = if stripes.is_empty() {
        seed.wrapping_add(P5)
    } else {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        for stripe in stripes {
            let (lanes, _) = stripe.as_chunks::<8>();
            for (acc, lane) in v.iter_mut().zip(lanes) {
                *acc = round(*acc, u64::from_le_bytes(*lane));
            }
        }
        let mixed = v[0]
            .rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18));
        v.iter().fold(mixed, |hash, &acc| merge(hash, acc))
    };
    hash = hash.wrapping_add(data.len() as u64);

    let (words, mut rest) = rest.as_chunks::<8>();
    for word in words {
        hash = (hash ^ round(0, u64::from_le_bytes(*word)))
            .rotate_left(27)
            .wrapping_mul(P1)
            .wrapping_add(P4);
    }
    if let Some((half, tail)) = rest.split_first_chunk::<4>() {
        hash = (hash ^ (u32::from_le_bytes(*half) as u64).wrapping_mul(P1))
            .rotate_left(23)
            .wrapping_mul(P2)
            .wrapping_add(P3);
        rest = tail;
    }
    for &byte in rest {
        hash = (hash ^ (byte as u64).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }

    hash ^= hash >> 33;
    hash = hash.wrapping_mul(P2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(P3);
    hash ^ (hash >> 32)
}

#[cfg(test)]
mod tests {
    use super::xxh64;

    #[test]
    fn known_values() {
        assert_eq!(xxh64(b"", 0), 0xEF46_DB37_51D8_E999);
        assert_eq!(xxh64(b"abc", 0), 0x44BC_2CF5_AD77_0999);
    }
}
