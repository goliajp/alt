use crate::rng::Rng;
use alt_json::{Map, Number, Value};

/// code points worth mixing into strings: escapes, controls, both ends of
/// every utf-8 length, the surrogate gap's neighbours, noncharacters
const NOTABLE: &[u32] = &[
    0x00, 0x01, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x1f, 0x20, 0x22, 0x2f, 0x5c, 0x7e,
    0x7f, 0x80, 0x9f, 0xa0, 0xff, 0x100, 0x300, 0x7ff, 0x800, 0x2028, 0x2029, 0x200d, 0xd7ff,
    0xe000, 0xfeff, 0xfffd, 0xfffe, 0xffff, 0x1_0000, 0x1_f600, 0xe_0001, 0xf_0000, 0x10_fffd,
    0x10_ffff,
];

pub fn scalar(rng: &mut Rng) -> char {
    loop {
        let code = match rng.below(8) {
            0 => *rng.pick(NOTABLE),
            1 => rng.below(0x20) as u32,
            2 | 3 => 0x20 + rng.below(0x5f) as u32,
            4 => rng.below(0x800) as u32,
            5 => rng.below(0x1_0000) as u32,
            6 => 0x4e00 + rng.below(0x5200) as u32,
            _ => rng.below(0x11_0000) as u32,
        };
        if let Some(c) = char::from_u32(code) {
            return c;
        }
    }
}

pub fn string(rng: &mut Rng, max_len: u64) -> String {
    let len = rng.below(max_len + 1);
    if rng.chance(3) {
        // plain ascii runs of every length exercise the word-at-a-time scan
        return (0..len)
            .map(|_| (0x20 + rng.below(0x5f) as u8) as char)
            .collect();
    }
    (0..len).map(|_| scalar(rng)).collect()
}

/// finite floats biased towards the places where formatting changes shape
pub fn float(rng: &mut Rng) -> f64 {
    loop {
        let f = match rng.below(6) {
            0 => f64::from_bits(rng.next()),
            1 => rng.range(-1_000_000, 1_000_000) as f64 / 1000.0,
            2 => rng.range(-10, 10) as f64,
            3 => (rng.next() >> rng.below(64)) as f64,
            4 => {
                let text = format!("{}e{}", rng.below(100_000), rng.range(-330, 310));
                text.parse().unwrap()
            }
            _ => f64::from_bits(rng.next() >> rng.below(64)),
        };
        if f.is_finite() {
            return f;
        }
    }
}

pub fn number(rng: &mut Rng) -> Number {
    match rng.below(6) {
        0 => Number::from(rng.below(1000)),
        1 => Number::from(rng.next() >> rng.below(64)),
        2 => Number::from(-((rng.next() >> 1 >> rng.below(63)) as i64) - 1),
        3 => Number::from(*rng.pick(&[0, u64::MAX, i64::MAX as u64, i64::MAX as u64 + 1])),
        4 => Number::from(*rng.pick(&[i64::MIN, i64::MIN + 1, -1])),
        _ => Number::from_f64(float(rng)).unwrap(),
    }
}

pub fn value(rng: &mut Rng, depth: u32) -> Value {
    let kinds = if depth == 0 { 5 } else { 8 };
    match rng.below(kinds) {
        0 => Value::Null,
        1 => Value::Bool(rng.chance(2)),
        2 | 3 => Value::Number(number(rng)),
        4 => Value::String(string(rng, 24)),
        5 | 6 => {
            let len = rng.below(6);
            Value::Array((0..len).map(|_| value(rng, depth - 1)).collect())
        }
        _ => {
            let len = rng.below(6);
            let mut map = Map::new();
            for _ in 0..len {
                map.insert(string(rng, 6), value(rng, depth - 1));
            }
            Value::Object(map)
        }
    }
}
