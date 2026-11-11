//! Expected values under `fixtures/` were produced by the argon2 crate (0.5) in the run where
//! it agreed with this implementation on every case; `reference.rs` says how to regenerate them.

use super::{hex, noise};
use alt_crypto::argon2::{self, Error, Params, PasswordHash};
use argon2::{hash_keyed_into, hash_password, hash_password_into};

#[test]
fn rfc_9106_argon2id_vector() {
    let mut tag = [0u8; 32];
    hash_keyed_into(
        &[1; 32],
        &[2; 16],
        &[3; 8],
        &[4; 12],
        &Params {
            m_cost: 32,
            t_cost: 3,
            p_cost: 4,
        },
        &mut tag,
    )
    .unwrap();
    assert_eq!(
        hex(&tag),
        "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"
    );
}

pub(crate) const PRODUCTION: Params = Params {
    m_cost: 65_536,
    t_cost: 3,
    p_cost: 1,
};

pub(crate) struct Case {
    pub params: Params,
    pub password: Vec<u8>,
    pub salt: Vec<u8>,
    pub tag_len: usize,
}

/// work factors with one and several lanes, memory that is and is not a
/// multiple of the lane count, and every input length class; the production
/// work factors come last
pub(crate) fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let mut number = 0u64;
    for (m_cost, t_cost, p_cost) in [
        (8, 1, 1),
        (9, 1, 1),
        (16, 2, 1),
        (32, 3, 4),
        (67, 1, 3),
        (100, 2, 2),
        (513, 1, 1),
        (1024, 3, 1),
        (1040, 2, 5),
        (4096, 1, 2),
    ] {
        for (password_len, salt_len, tag_len) in [
            (0, 8, 4),
            (1, 16, 32),
            (12, 9, 64),
            (64, 16, 65),
            (200, 64, 100),
            (1024, 16, 32),
        ] {
            number += 1;
            cases.push(Case {
                params: Params {
                    m_cost,
                    t_cost,
                    p_cost,
                },
                password: noise(password_len, number),
                salt: noise(salt_len, number + 1000),
                tag_len,
            });
        }
    }
    cases.push(Case {
        params: PRODUCTION,
        password: b"a real acceptance test password".to_vec(),
        salt: noise(16, 5),
        tag_len: 32,
    });
    cases
}

#[test]
fn tags_match_the_recorded_ones() {
    // the tags of `cases()` one after another, written by `reference.rs`
    let mut expected = &include_bytes!("fixtures/argon2_tags.bin")[..];
    for (index, case) in cases().iter().enumerate() {
        let (tag, rest) = expected.split_at(case.tag_len);
        expected = rest;
        let mut ours = vec![0; case.tag_len];
        hash_password_into(&case.password, &case.salt, &case.params, &mut ours).unwrap();
        assert_eq!(ours, tag, "case {index}: {:?}", case.params);
    }
    assert!(expected.is_empty());
}

#[test]
fn fixed_size_and_slice_outputs_agree() {
    let params = Params {
        m_cost: 16,
        t_cost: 1,
        p_cost: 1,
    };
    let fixed: [u8; 24] = hash_password(b"password", &[7; 8], &params).unwrap();
    let mut slice = [0u8; 24];
    hash_password_into(b"password", &[7; 8], &params, &mut slice).unwrap();
    assert_eq!(fixed, slice);
}

#[test]
fn rejects_inputs_outside_the_specification() {
    let ok = Params {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    };
    let run = |params: Params, salt: &[u8], len: usize| {
        hash_password_into(b"password", salt, &params, &mut vec![0; len])
    };
    assert_eq!(run(ok, &[0; 8], 4), Ok(()));
    assert_eq!(
        run(Params { m_cost: 7, ..ok }, &[0; 8], 4),
        Err(Error::Params)
    );
    assert_eq!(
        run(
            Params {
                m_cost: 15,
                p_cost: 2,
                ..ok
            },
            &[0; 8],
            4
        ),
        Err(Error::Params)
    );
    assert_eq!(
        run(Params { t_cost: 0, ..ok }, &[0; 8], 4),
        Err(Error::Params)
    );
    assert_eq!(
        run(Params { p_cost: 0, ..ok }, &[0; 8], 4),
        Err(Error::Params)
    );
    assert_eq!(
        run(
            Params {
                m_cost: u32::MAX,
                p_cost: 1 << 24,
                ..ok
            },
            &[0; 8],
            4
        ),
        Err(Error::Params)
    );
    assert_eq!(run(ok, &[0; 7], 4), Err(Error::Salt));
    assert_eq!(run(ok, &[0; 8], 3), Err(Error::OutputLength));
}

/// the password behind line `index` of `fixtures/argon2_phc.txt`
pub(crate) fn sample_password(index: usize) -> String {
    format!("sample password number {index}")
}

/// (work factors, salt length) of each line of `fixtures/argon2_phc.txt`; the
/// production lines are made the way alt-browser-auth made the ones in databases
pub(crate) const PHC_SAMPLES: [(Params, usize); 9] = [
    (PRODUCTION, 16),
    (PRODUCTION, 16),
    (
        Params {
            m_cost: 64,
            t_cost: 2,
            p_cost: 1,
        },
        8,
    ),
    (
        Params {
            m_cost: 64,
            t_cost: 2,
            p_cost: 1,
        },
        9,
    ),
    (
        Params {
            m_cost: 64,
            t_cost: 2,
            p_cost: 1,
        },
        10,
    ),
    (
        Params {
            m_cost: 64,
            t_cost: 2,
            p_cost: 1,
        },
        16,
    ),
    (
        Params {
            m_cost: 256,
            t_cost: 1,
            p_cost: 2,
        },
        24,
    ),
    (
        Params {
            m_cost: 256,
            t_cost: 1,
            p_cost: 2,
        },
        32,
    ),
    (
        Params {
            m_cost: 19_456,
            t_cost: 2,
            p_cost: 1,
        },
        48,
    ),
];

#[test]
fn recorded_phc_strings_parse_verify_and_are_reproduced() {
    // one string per line, written by `reference.rs` with the argon2 crate's own encoder
    let samples: Vec<&str> = include_str!("fixtures/argon2_phc.txt").lines().collect();
    assert_eq!(samples.len(), PHC_SAMPLES.len());
    for (index, (encoded, (params, salt_len))) in samples.iter().zip(PHC_SAMPLES).enumerate() {
        let password = sample_password(index);
        let parsed = PasswordHash::parse(encoded).unwrap();
        assert_eq!(parsed.params, params, "line {index}");
        assert_eq!(parsed.salt.len(), salt_len, "line {index}");
        assert_eq!(parsed.hash.len(), 32, "line {index}");
        assert_eq!(parsed.to_string(), *encoded, "line {index}");
        assert_eq!(parsed.verify(password.as_bytes()), Ok(true), "line {index}");
        if params == PRODUCTION {
            // one pass over 64 MiB per check: the lighter samples cover the other paths
            continue;
        }
        let wrong = sample_password(index + 1);
        assert_eq!(parsed.verify(wrong.as_bytes()), Ok(false), "line {index}");
        let ours = PasswordHash::new(password.as_bytes(), &parsed.salt, &params, 32).unwrap();
        assert_eq!(ours.to_string(), *encoded, "line {index}");
    }
    assert!(samples[0].starts_with("$argon2id$v=19$m=65536,t=3,p=1$"));
}

#[test]
fn phc_parsing_rejects_everything_but_the_canonical_form() {
    let params = Params {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    };
    let good = PasswordHash::new(b"password", &[9; 16], &params, 32)
        .unwrap()
        .to_string();
    assert_eq!(
        PasswordHash::parse(&good).unwrap().verify(b"password"),
        Ok(true)
    );
    let reordered = good.replace("m=8,t=1,p=1", "p=1,m=8,t=1");
    assert_eq!(
        PasswordHash::parse(&reordered).unwrap().verify(b"password"),
        Ok(true)
    );
    let (head, tag) = good.rsplit_once('$').unwrap();
    for bad in [
        String::new(),
        "invalid".into(),
        good[1..].into(),
        good.replace("argon2id", "argon2i"),
        good.replace("argon2id", "argon2d"),
        good.replace("v=19", "v=16"),
        good.replace("$v=19", ""),
        good.replace("m=8", "m=08"),
        good.replace("m=8", "m=+8"),
        good.replace("m=8", "m="),
        good.replace("m=8", "m=4294967296"),
        good.replace("m=8,", ""),
        good.replace("t=1", "t=1,t=1"),
        good.replace("p=1", "p=1,keyid=AAAA"),
        good.replace("p=1", "p=1,"),
        format!("{good}$"),
        format!("{good}="),
        format!("{good}$AAAA"),
        format!("{head}$"),
        format!("{head}${tag}!"),
        format!("{head}$-{}", &tag[1..]),
        head.to_string(),
        format!(" {good}"),
        format!("{good}\n"),
    ] {
        assert_eq!(PasswordHash::parse(&bad), Err(Error::Encoding), "{bad:?}");
    }
    // a parsed string whose work factors cannot be computed fails at verification
    let impossible = PasswordHash::parse(&good.replace("m=8", "m=7")).unwrap();
    assert_eq!(impossible.verify(b"password"), Err(Error::Params));
}

#[test]
fn accelerated_compression_matches_portable() {
    use alt_crypto::argon2::block::{self, Block};
    println!("argon2 block: {}", block::implementation());
    let fill = |seed: u64| -> Block {
        let bytes = noise(1024, seed);
        std::array::from_fn(|i| u64::from_le_bytes(super::array(&bytes[8 * i..8 * i + 8])))
    };
    for case in 1..40u64 {
        // all-ones words make every 32-bit product and carry as large as it can be
        let previous = if case == 1 {
            [u64::MAX; 128]
        } else {
            fill(3 * case)
        };
        let reference = if case <= 2 {
            [u64::MAX >> 1; 128]
        } else {
            fill(3 * case + 1)
        };
        for xor in [false, true] {
            let mut fast = fill(3 * case + 2);
            let mut slow = fast;
            block::compress(&previous, &reference, &mut fast, xor);
            block::portable(&previous, &reference, &mut slow, xor);
            assert_eq!(fast, slow, "case {case}, xor {xor}");
        }
    }
}
