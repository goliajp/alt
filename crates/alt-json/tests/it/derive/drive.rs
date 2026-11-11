//! Turns one random value into many inputs: its own text, every way of
//! cutting it short, byte-level damage, and edits to its structure.

use alt_json::{Map, Value};

use super::support::{Both, Tally, input, output};
use crate::generate;
use crate::invalid::mutate;
use crate::rng::Rng;

/// a scalar of a random kind, to put where another kind is expected
fn stray(rng: &mut Rng) -> Value {
    match rng.below(12) {
        0 => Value::Null,
        1 => Value::Bool(rng.chance(2)),
        2 => Value::from(rng.below(300)),
        3 => Value::from(-(rng.below(300) as i64) - 1),
        4 => Value::from(rng.below(100) as f64 + 0.5),
        5 => Value::from(*rng.pick(&[u64::MAX, 1 << 63, 1 << 32, 65536, 256, 128])),
        6 => Value::from(*rng.pick(&[i64::MIN, -129, -32769, -2147483649])),
        7 => Value::from(*rng.pick(&[1e300, -1e300, 1e40, 0.0, -0.0, 3.0])),
        8 => Value::String(generate::string(rng, 6)),
        9 => Value::from(*rng.pick(&["", "0", "12", "007", "-1", "true", "null", "a"])),
        10 => Value::Array(vec![]),
        _ => Value::Object(Map::new()),
    }
}

/// One structural change somewhere in the tree.
fn edit(rng: &mut Rng, value: &mut Value) {
    let descend = match value {
        Value::Array(items) => !items.is_empty() && !rng.chance(3),
        Value::Object(map) => !map.is_empty() && !rng.chance(3),
        _ => false,
    };
    if descend {
        match value {
            Value::Array(items) => {
                let at = rng.below(items.len() as u64) as usize;
                edit(rng, &mut items[at]);
            }
            Value::Object(map) => {
                let at = rng.below(map.len() as u64) as usize;
                if let Some(member) = map.values_mut().nth(at) {
                    edit(rng, member);
                }
            }
            _ => {}
        }
        return;
    }
    match value {
        Value::Object(map) => match rng.below(6) {
            0 if !map.is_empty() => {
                let at = rng.below(map.len() as u64) as usize;
                let key = map.keys().nth(at).cloned().unwrap_or_default();
                map.remove(&key);
            }
            1 => {
                map.insert(
                    rng.pick(&["extra", "kind", "value", "a", ""]).to_string(),
                    stray(rng),
                );
            }
            2 => {
                map.insert("extra".to_owned(), generate::value(rng, 2));
            }
            3 if !map.is_empty() => {
                // a member moves to another name
                let at = rng.below(map.len() as u64) as usize;
                let key = map.keys().nth(at).cloned().unwrap_or_default();
                if let Some(member) = map.remove(&key) {
                    map.insert(key.to_uppercase(), member);
                }
            }
            4 => *value = Value::Array(std::mem::take(map).into_values().collect()),
            _ => *value = stray(rng),
        },
        Value::Array(items) => match rng.below(5) {
            0 if !items.is_empty() => {
                let at = rng.below(items.len() as u64) as usize;
                items.remove(at);
            }
            1 => items.push(stray(rng)),
            2 if !items.is_empty() => {
                let at = rng.below(items.len() as u64) as usize;
                let copy = items[at].clone();
                items.push(copy);
            }
            3 => items.insert(0, stray(rng)),
            _ => *value = stray(rng),
        },
        _ => *value = stray(rng),
    }
}

/// Repeats the first key of some object in the text, with another value.
fn duplicate_key(rng: &mut Rng, text: &[u8]) -> Option<Vec<u8>> {
    let opens: Vec<usize> = (0..text.len()).filter(|&i| text[i] == b'{').collect();
    let open = *opens.get(rng.below(opens.len().max(1) as u64) as usize)?;
    let key_start = open + 1;
    if text.get(key_start) != Some(&b'"') {
        return None;
    }
    let key_len = text[key_start + 1..].iter().position(|&b| b == b'"')?;
    let key = &text[key_start..key_start + key_len + 2];
    let value = alt_json::to_vec(&stray(rng)).ok()?;
    let mut edited = text[..key_start].to_vec();
    if rng.chance(2) {
        // the repeat goes first
        edited.extend_from_slice(key);
        edited.push(b':');
        edited.extend_from_slice(&value);
        edited.push(b',');
        edited.extend_from_slice(&text[key_start..]);
    } else {
        // the repeat follows the original, with the original's value
        let after_key = key_start + key.len();
        let end = after_key
            + text[after_key..]
                .iter()
                .position(|&b| b == b',' || b == b'}')?;
        edited.extend_from_slice(&text[key_start..end]);
        edited.push(b',');
        edited.extend_from_slice(&text[key_start..]);
    }
    Some(edited)
}

/// Compares both libraries on `rounds` random values of `T` and on the
/// inputs derived from each.
pub fn exercise<T: Both>(seed: u64, rounds: u64) -> Tally {
    let mut rng = Rng::new(seed);
    let mut tally = Tally::default();
    for _ in 0..rounds {
        let value = T::arbitrary(&mut rng);
        let Some(compact) = output(&value) else {
            tally.unwritable += 1;
            continue;
        };
        tally.values += 1;
        input::<T>(&compact, &mut tally);
        input::<T>(&alt_json::to_vec_pretty(&value).unwrap(), &mut tally);

        if compact.len() <= 200 {
            for end in 0..compact.len() {
                input::<T>(&compact[..end], &mut tally);
            }
        } else {
            for _ in 0..40 {
                let end = rng.below(compact.len() as u64) as usize;
                input::<T>(&compact[..end], &mut tally);
            }
        }
        for _ in 0..8 {
            let mut damaged = compact.clone();
            for _ in 0..1 + rng.below(2) {
                mutate(&mut rng, &mut damaged);
            }
            input::<T>(&damaged, &mut tally);
        }
        let tree = alt_json::parse(&compact).unwrap();
        // key order as a tree has it, not as the type declares it
        input::<T>(&alt_json::to_vec(&tree).unwrap(), &mut tally);
        for _ in 0..12 {
            let mut edited = tree.clone();
            for _ in 0..1 + rng.below(2) {
                edit(&mut rng, &mut edited);
            }
            input::<T>(&alt_json::to_vec(&edited).unwrap(), &mut tally);
        }
        for _ in 0..3 {
            if let Some(repeated) = duplicate_key(&mut rng, &compact) {
                input::<T>(&repeated, &mut tally);
            }
        }
    }
    tally
}
