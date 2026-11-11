use crate::check::{parse, report};
use crate::generate;
use crate::rng::Rng;

const SEEDS: &[&str] = &[
    r#"{"a":1,"b":[true,false,null],"c":{"d":"e"}}"#,
    r#"[1,-2,3.5,4e5,-6.7E-8,0,-0,0.0,1e0]"#,
    r#"["a\nb","é😀","\"\\\/\b\f\r\t",""]"#,
    r#"{"key with space":{"nested":[[[]]],"empty":{}},"n":18446744073709551615}"#,
    "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": \"x\"\n}",
    r#"[[[[[[[[[[1]]]]]]]]]]"#,
    r#"{"a":{"a":{"a":{"a":{"a":null}}}}}"#,
    " \t\r\n[ 1 , \"two\" , { \"three\" : 3.0 } ] \n",
    r#""just a string with é and 😀""#,
    "123456789012345678901234567890.123456789e-10",
    "-9223372036854775808",
    "true",
];

const INTERESTING: &[u8] = b"[]{}:,\"\\ \n\t0123456789-+.eEtfnul'/\x00\x1f\x7f\x80\xc3\xe2\xf0\xff";

/// every prefix of every seed
#[test]
fn truncation() {
    let mut rng = Rng::new(0x7c);
    let (mut cases, mut accepted) = (0, 0);
    let mut documents: Vec<Vec<u8>> = SEEDS.iter().map(|s| s.as_bytes().to_vec()).collect();
    for _ in 0..3000 {
        let value = generate::value(&mut rng, 4);
        documents.push(alt_json::to_vec(&value));
        documents.push(alt_json::to_vec_pretty(&value));
    }
    for document in &documents {
        for end in 0..=document.len() {
            accepted += u64::from(parse(&document[..end]));
            cases += 1;
        }
    }
    report("invalid, truncation", cases, Some(accepted));
}

/// every byte of every seed replaced by, and preceded by, each interesting
/// byte; and every byte deleted
#[test]
fn single_byte_edits_of_seeds() {
    let (mut cases, mut accepted) = (0, 0);
    for seed in SEEDS {
        let seed = seed.as_bytes();
        for at in 0..seed.len() {
            let mut deleted = seed.to_vec();
            deleted.remove(at);
            accepted += u64::from(parse(&deleted));
            cases += 1;
            for &byte in INTERESTING {
                let mut replaced = seed.to_vec();
                replaced[at] = byte;
                accepted += u64::from(parse(&replaced));
                let mut inserted = seed.to_vec();
                inserted.insert(at, byte);
                accepted += u64::from(parse(&inserted));
                cases += 2;
            }
        }
    }
    report("invalid, single byte edits of seeds", cases, Some(accepted));
}

fn mutate(rng: &mut Rng, document: &mut Vec<u8>) {
    let byte = if rng.chance(4) {
        rng.next() as u8
    } else {
        *rng.pick(INTERESTING)
    };
    let at = rng.below(document.len() as u64 + 1) as usize;
    match rng.below(7) {
        0 | 1 if at < document.len() => document[at] = byte,
        2 if at < document.len() => {
            document.remove(at);
        }
        3 if at < document.len() => document[at] ^= 1 << rng.below(8),
        4 => {
            // repeat a slice
            let end = (at + 1 + rng.below(6) as usize).min(document.len());
            let slice = document[at..end].to_vec();
            document.splice(at..at, slice);
        }
        5 if at < document.len() => {
            // swap two bytes
            let other = rng.below(document.len() as u64) as usize;
            document.swap(at, other);
        }
        _ => document.insert(at, byte),
    }
}

/// one to three random edits of random documents
fn random_edits(seed: u64, count: u64) -> (u64, u64) {
    let mut rng = Rng::new(seed);
    let (mut cases, mut accepted) = (0, 0);
    while cases < count {
        let value = generate::value(&mut rng, 4);
        let original = if rng.chance(3) {
            alt_json::to_vec_pretty(&value)
        } else {
            alt_json::to_vec(&value)
        };
        for _ in 0..20 {
            let mut document = original.clone();
            for _ in 0..1 + rng.below(3) {
                mutate(&mut rng, &mut document);
            }
            accepted += u64::from(parse(&document));
            cases += 1;
        }
    }
    (cases, accepted)
}

#[test]
fn random_edits_of_random_documents() {
    let (cases, accepted) = random_edits(0xed17, 3_000_000);
    report(
        "invalid, random edits of random documents",
        cases,
        Some(accepted),
    );
}

#[test]
#[ignore = "100 million edited documents on all cores, needs --release"]
fn random_edits_of_random_documents_long() {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as u64;
    let per_thread = 100_000_000 / threads;
    let (cases, accepted) = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| scope.spawn(move || random_edits(0x1e17 + t, per_thread)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .fold((0, 0), |sum, part| (sum.0 + part.0, sum.1 + part.1))
    });
    report(
        "invalid, random edits of random documents (long)",
        cases,
        Some(accepted),
    );
}

/// random bytes from the structural alphabet, no valid document underneath
#[test]
fn random_token_soup() {
    let mut rng = Rng::new(0x50);
    let (mut cases, mut accepted) = (0, 0);
    let tokens: &[&[u8]] = &[
        b"[",
        b"]",
        b"{",
        b"}",
        b":",
        b",",
        b"\"",
        b"\"a\"",
        b"\"\"",
        b"null",
        b"true",
        b"false",
        b"0",
        b"1",
        b"-1",
        b"1.5",
        b"1e5",
        b" ",
        b"\n",
        b"\\",
        b"\\u0041",
        b"\\ud83d",
        b"\\ude00",
        b"-",
        b".",
        b"e",
        b"\xc3\xa9",
        b"\xff",
        b"\x00",
        b"nul",
        b"00",
    ];
    for _ in 0..2_000_000 {
        let mut document = Vec::new();
        for _ in 0..1 + rng.below(12) {
            document.extend_from_slice(rng.pick(tokens));
        }
        accepted += u64::from(parse(&document));
        cases += 1;
    }
    report("invalid, random token soup", cases, Some(accepted));
}
