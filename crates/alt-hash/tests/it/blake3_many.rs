use crate::noise;
use alt_hash::blake3::{self, Backend};

/// lengths where the number of blocks or chunks changes
const EDGES: [usize; 22] = [
    0, 1, 63, 64, 65, 127, 128, 129, 511, 512, 513, 959, 960, 961, 1023, 1024, 1025, 2048, 3072,
    3600, 4096, 9000,
];

fn check(data: &[u8], lengths: &[usize], next: &mut impl FnMut(usize) -> usize) {
    // inputs that start anywhere, as separate objects do
    let inputs: Vec<&[u8]> = lengths
        .iter()
        .map(|len| {
            let start = next(data.len() - len);
            &data[start..start + len]
        })
        .collect();
    let expected: Vec<_> = inputs
        .iter()
        .map(|input| blake3::hash_with(Backend::portable(), input))
        .collect();
    assert_eq!(blake3::hash_many(&inputs), expected, "lengths {lengths:?}");
    for backend in Backend::supported() {
        assert_eq!(
            blake3::hash_many_with(backend, &inputs),
            expected,
            "{} lengths {lengths:?}",
            backend.name()
        );
    }
}

#[test]
fn blake3_many_inputs_match_one_by_one() {
    let data = noise(1 << 16, 0x0f1e_2d3c_4b5a_6978);
    let mut random = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |below: usize| {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        (random % (below as u64 + 1)) as usize
    };
    check(&data, &[], &mut next);
    // every count of equal inputs up to more than two groups of the widest lanes
    for len in EDGES {
        for count in 1..=17 {
            check(&data, &vec![len; count], &mut next);
        }
    }
    // every length of one chunk, alone and among others of that length
    for len in 0..=1024 {
        check(&data, &[len], &mut next);
        check(&data, &[len, len, len, len, len], &mut next);
    }
    // mixed batches: any lengths, then lengths near each other, then the edges
    for round in 0..600 {
        let count = next(40);
        let lengths: Vec<usize> = (0..count)
            .map(|_| match round % 3 {
                0 => next(5000),
                1 => 448 + next(130),
                _ => EDGES[next(EDGES.len() - 1)],
            })
            .collect();
        check(&data, &lengths, &mut next);
    }
}
