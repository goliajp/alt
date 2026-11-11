use super::{hex, noise};
use alt_crypto::argon2::{self, Error, Params, PasswordHash};
use argon2::{hash_keyed_into, hash_password, hash_password_into};

fn reference(password: &[u8], salt: &[u8], params: &Params, len: usize) -> Vec<u8> {
    let hasher = ::argon2::Argon2::new(
        ::argon2::Algorithm::Argon2id,
        ::argon2::Version::V0x13,
        ::argon2::Params::new(params.m_cost, params.t_cost, params.p_cost, Some(len)).unwrap(),
    );
    let mut out = vec![0; len];
    hasher.hash_password_into(password, salt, &mut out).unwrap();
    out
}

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

#[test]
fn matches_argon2_crate_across_work_factors_and_lengths() {
    let mut case = 0u64;
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
        let params = Params {
            m_cost,
            t_cost,
            p_cost,
        };
        for (password_len, salt_len, tag_len) in [
            (0, 8, 4),
            (1, 16, 32),
            (12, 9, 64),
            (64, 16, 65),
            (200, 64, 100),
            (1024, 16, 32),
        ] {
            case += 1;
            let password = noise(password_len, case);
            let salt = noise(salt_len, case + 1000);
            let mut ours = vec![0; tag_len];
            hash_password_into(&password, &salt, &params, &mut ours).unwrap();
            assert_eq!(
                ours,
                reference(&password, &salt, &params, tag_len),
                "m={m_cost} t={t_cost} p={p_cost} password={password_len} salt={salt_len} tag={tag_len}"
            );
        }
    }
}

#[test]
fn matches_argon2_crate_at_the_production_work_factors() {
    let params = Params {
        m_cost: 65_536,
        t_cost: 3,
        p_cost: 1,
    };
    let salt = noise(16, 5);
    let ours: [u8; 32] = hash_password(b"a real acceptance test password", &salt, &params).unwrap();
    assert_eq!(
        ours.to_vec(),
        reference(b"a real acceptance test password", &salt, &params, 32)
    );
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

#[test]
fn phc_strings_are_interchangeable_with_the_argon2_crate() {
    use ::argon2::{
        PasswordHasher, PasswordVerifier,
        password_hash::{PasswordHash as TheirHash, SaltString},
    };
    let theirs = ::argon2::Argon2::new(
        ::argon2::Algorithm::Argon2id,
        ::argon2::Version::V0x13,
        ::argon2::Params::new(65_536, 3, 1, Some(32)).unwrap(),
    );
    let params = Params {
        m_cost: 65_536,
        t_cost: 3,
        p_cost: 1,
    };
    let password = b"a real acceptance test password";
    let salt = noise(16, 77);

    // the way alt-browser-auth produced the strings now in databases
    let encoded = theirs
        .hash_password(password, &SaltString::encode_b64(&salt).unwrap())
        .unwrap()
        .to_string();
    let parsed = PasswordHash::parse(&encoded).unwrap();
    assert_eq!(parsed.params, params);
    assert_eq!(parsed.salt, salt);
    assert_eq!(parsed.hash.len(), 32);
    assert_eq!(parsed.to_string(), encoded);
    assert_eq!(parsed.verify(password), Ok(true));
    assert_eq!(parsed.verify(b"a real acceptance test passwore"), Ok(false));

    let ours = PasswordHash::new(password, &salt, &params, 32)
        .unwrap()
        .to_string();
    assert_eq!(ours, encoded);
    let ours = PasswordHash::new(password, &noise(16, 78), &params, 32)
        .unwrap()
        .to_string();
    assert!(ours.starts_with("$argon2id$v=19$m=65536,t=3,p=1$"));
    let parsed = TheirHash::new(&ours).unwrap();
    assert!(theirs.verify_password(password, &parsed).is_ok());
    assert!(theirs.verify_password(b"incorrect", &parsed).is_err());
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
        format!("{head}${}", tag.replace(['+', '/'], "-")),
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
