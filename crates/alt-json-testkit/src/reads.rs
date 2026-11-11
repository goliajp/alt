//! Frozen reading behaviour of a type: inputs with what serde made of them.
//!
//! One line per input, in three tab-separated parts. `A`, the text, and the
//! compact text of the value that was read, written back; or `R`, the text,
//! and the message of the error. A type that is only ever read needs a
//! writer for this in its tests alone.

use alt_json::{FromJson, ToJson};
use serde::{Serialize, de::DeserializeOwned};

use crate::drive::{duplicate_key, edit, mutate};
use crate::rng::Rng;

fn fits_a_line(text: &[u8]) -> bool {
    std::str::from_utf8(text).is_ok_and(|text| !text.contains(['\t', '\n', '\r']))
}

/// Builds the fixture of `T` with serde from every example and from
/// `samples` texts that `make` returns, each also restructured, damaged and
/// given a repeated key. An example need not be JSON.
pub fn record<T: Serialize + DeserializeOwned>(
    seed: u64,
    examples: &[&str],
    samples: u64,
    make: impl Fn(&mut Rng) -> Vec<u8>,
) -> String {
    let mut rng = Rng::new(seed);
    let mut starts: Vec<Vec<u8>> = examples
        .iter()
        .map(|text| text.as_bytes().to_vec())
        .collect();
    for _ in 0..samples {
        starts.push(make(&mut rng));
    }
    let mut lines = Vec::new();
    let verdict = |text: Vec<u8>, lines: &mut Vec<String>| {
        if !fits_a_line(&text) {
            return;
        }
        let (mark, outcome) = match serde_json::from_slice::<T>(&text) {
            Ok(value) => ('A', serde_json::to_string(&value).unwrap()),
            Err(error) => ('R', error.to_string()),
        };
        assert!(fits_a_line(outcome.as_bytes()), "{outcome}");
        lines.push(format!(
            "{mark}\t{}\t{outcome}",
            String::from_utf8(text).unwrap()
        ));
    };
    for start in starts {
        verdict(start.clone(), &mut lines);
        if let Ok(tree) = alt_json::parse(&start) {
            verdict(alt_json::to_vec(&tree).unwrap(), &mut lines);
            for _ in 0..8 {
                let mut edited = tree.clone();
                for _ in 0..1 + rng.below(2) {
                    edit(&mut rng, &mut edited);
                }
                verdict(alt_json::to_vec(&edited).unwrap(), &mut lines);
            }
        }
        for _ in 0..3 {
            let mut damaged = start.clone();
            mutate(&mut rng, &mut damaged);
            verdict(damaged, &mut lines);
        }
        if let Some(repeated) = duplicate_key(&mut rng, &start) {
            verdict(repeated, &mut lines);
        }
    }
    lines.sort();
    lines.dedup();
    lines.join("\n") + "\n"
}

/// What a fixture held.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub accepted: usize,
    pub rejected: usize,
}

/// Holds `T` to a fixture made by [`record`].
pub fn verify<T: ToJson + FromJson>(fixture: &str) -> Counts {
    let mut counts = Counts::default();
    for line in fixture.lines() {
        let mut parts = line.splitn(3, '\t');
        let (mark, text, outcome) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        match (mark, alt_json::from_str::<T>(text)) {
            ("A", Ok(value)) => {
                assert_eq!(alt_json::to_string(&value).unwrap(), outcome, "{text}");
                counts.accepted += 1;
            }
            ("R", Err(error)) => {
                assert_eq!(error.to_string(), outcome, "{text}");
                counts.rejected += 1;
            }
            ("A", Err(error)) => panic!("must be accepted: {text}: {error}"),
            ("R", Ok(_)) => panic!("must be rejected with {outcome:?}: {text}"),
            _ => panic!("malformed fixture line: {line}"),
        }
    }
    counts
}

/// Holds `T` to the stored fixture `<dir>/<name>.txt`.
pub fn verify_file<T: ToJson + FromJson>(dir: &str, name: &str) -> Counts {
    let counts = verify::<T>(&std::fs::read_to_string(format!("{dir}/{name}.txt")).unwrap());
    assert!(counts.accepted > 0, "{name}: nothing accepted");
    assert!(counts.rejected > 0, "{name}: nothing rejected");
    println!(
        "{name}: {} accepted, {} rejected",
        counts.accepted, counts.rejected
    );
    counts
}

/// Writes the fixture `<dir>/<name>.txt` that [`record`] builds.
pub fn record_file<T: Serialize + DeserializeOwned>(
    dir: &str,
    name: &str,
    seed: u64,
    examples: &[&str],
    samples: u64,
    make: impl Fn(&mut Rng) -> Vec<u8>,
) {
    std::fs::create_dir_all(dir).unwrap();
    let recorded = record::<T>(seed, examples, samples, make);
    std::fs::write(format!("{dir}/{name}.txt"), recorded).unwrap();
}

/// The text serde writes for a random value generated from `T` itself.
pub fn from_type<T: Serialize + DeserializeOwned>(rng: &mut Rng) -> Vec<u8> {
    let value: T = crate::arbitrary::from_serde(rng);
    serde_json::to_vec(&value).expect("fixture values must be writable")
}
