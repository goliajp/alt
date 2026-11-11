use crate::check::{parse, report};
use crate::rng::Rng;

fn digits(rng: &mut Rng, count: u64) -> String {
    (0..count)
        .map(|_| (b'0' + rng.below(10) as u8) as char)
        .collect()
}

fn finite(rng: &mut Rng) -> f64 {
    loop {
        let f = f64::from_bits(rng.next_u64());
        if f.is_finite() {
            return f;
        }
    }
}

/// the shortest text of a random float, as both writers produce it
fn shortest_texts(seed: u64, count: u64) -> (u64, u64) {
    let mut rng = Rng::new(seed);
    let mut accepted = 0;
    for _ in 0..count {
        let text = serde_json::to_string(&finite(&mut rng)).unwrap();
        accepted += u64::from(parse(text.as_bytes()));
    }
    (count, accepted)
}

#[test]
fn shortest_text_of_random_floats() {
    let (cases, accepted) = shortest_texts(0x5407, 2_000_000);
    report(
        "float parse, shortest text of random floats",
        cases,
        Some(accepted),
    );
}

#[test]
#[ignore = "200 million texts on all cores, needs --release"]
fn shortest_text_of_random_floats_long() {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as u64;
    let per_thread = 200_000_000 / threads;
    let (cases, accepted) = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| scope.spawn(move || shortest_texts(0x10b6 + t, per_thread)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .fold((0, 0), |sum, part| (sum.0 + part.0, sum.1 + part.1))
    });
    report(
        "float parse, shortest text of random floats (long)",
        cases,
        Some(accepted),
    );
}

/// more digits than a float holds, in exponent and positional notation
#[test]
fn long_text_of_random_floats() {
    let mut rng = Rng::new(0x1097);
    let (mut cases, mut accepted) = (0, 0);
    for _ in 0..1_000_000 {
        let f = finite(&mut rng);
        let precision = rng.below(30) as usize;
        let text = format!("{f:.precision$e}");
        accepted += u64::from(parse(text.as_bytes()));
        cases += 1;
        if (1e-30..1e30).contains(&f.abs()) {
            let text = format!("{f:.precision$}");
            accepted += u64::from(parse(text.as_bytes()));
            cases += 1;
        }
    }
    report(
        "float parse, long text of random floats",
        cases,
        Some(accepted),
    );
}

/// digit strings of every shape: up to 30 integer digits, up to 30 fraction
/// digits, exponents from tiny to far out of range
#[test]
fn random_digit_strings() {
    let mut rng = Rng::new(0xd161);
    let (mut cases, mut accepted) = (0, 0);
    for _ in 0..2_000_000 {
        let mut text = String::new();
        if rng.chance(3) {
            text.push('-');
        }
        match rng.below(8) {
            0 => text.push('0'),
            _ => {
                text.push((b'1' + rng.below(9) as u8) as char);
                let count = rng.below(30);
                text.push_str(&digits(&mut rng, count));
            }
        }
        if rng.chance(2) {
            text.push('.');
            let count = 1 + rng.below(30);
            text.push_str(&digits(&mut rng, count));
        }
        if rng.chance(2) {
            text.push(*rng.pick(&['e', 'E']));
            text.push_str(rng.pick(&["", "+", "-"]));
            let exponent = match rng.below(10) {
                0 => rng.next_u64().to_string(),
                1 => format!("000{}", rng.below(400)),
                2 => rng.range(280, 420).to_string(),
                _ => rng.below(400).to_string(),
            };
            text.push_str(&exponent);
        }
        accepted += u64::from(parse(text.as_bytes()));
        cases += 1;
    }
    report("float parse, random digit strings", cases, Some(accepted));
}

/// every significand length around the capacity of u64, times every exponent
/// around the ends of the float range
#[test]
fn range_ends() {
    let mut rng = Rng::new(0xe4d);
    let (mut cases, mut accepted) = (0, 0);
    let significands = [
        "0",
        "1",
        "9",
        "17976931348623157",
        "17976931348623158",
        "17976931348623159",
        "18",
        "4940656458412465",
        "2470328229206232",
        "2470328229206233",
        "22250738585072014",
        "18446744073709551615",
        "18446744073709551616",
        "1844674407370955161",
        "99999999999999999999",
        "123456789012345678901234567890",
    ];
    for exponent in (-420..=-280).chain(-30..=30).chain(270..=420) {
        for significand in significands {
            for text in [
                format!("{significand}e{exponent}"),
                format!("-{significand}E{exponent}"),
                format!("{significand}.{}e{exponent}", digits(&mut rng, 5)),
                format!("0.{significand}e{exponent}"),
                format!("{}.{significand}e{exponent}", digits(&mut rng, 1)),
            ] {
                accepted += u64::from(parse(text.as_bytes()));
                cases += 1;
            }
        }
    }
    report("float parse, range ends", cases, Some(accepted));
}

#[test]
fn listed_numbers() {
    let texts = [
        "0",
        "-0",
        "0.0",
        "-0.0",
        "0e0",
        "0E0",
        "0e+0",
        "0e-0",
        "-0e0",
        "0.0e0",
        "00",
        "01",
        "-01",
        "-00",
        "0.",
        "0.e1",
        "1.",
        "1.e1",
        ".5",
        "-.5",
        "-",
        "+1",
        "1e",
        "1e+",
        "1e-",
        "1E",
        "1ee1",
        "1e1.5",
        "1.5.5",
        "1e1e1",
        "0x10",
        "1_000",
        "Infinity",
        "-Infinity",
        "NaN",
        "inf",
        "nan",
        "1e308",
        "1e309",
        "-1e309",
        "1.7976931348623157e308",
        "1.7976931348623159e308",
        "17976931348623157e292",
        "17976931348623159e292",
        "0.17976931348623157e309",
        "1e-323",
        "1e-324",
        "4.9e-324",
        "2.4e-324",
        "2.5e-324",
        "5e-324",
        "1e-400",
        "-1e-400",
        "1e-99999",
        "0e99999",
        "0e999999999999999999999",
        "-0e999999999999999999999",
        "0e-999999999999999999999",
        "1e999999999999999999999",
        "1e-999999999999999999999",
        "-1e-999999999999999999999",
        "0.0e999999999999999999999",
        "0.000e2147483647",
        "0e2147483647",
        "0e2147483648",
        "1e2147483647",
        "1e2147483648",
        "1e-2147483647",
        "1e-2147483648",
        "1e-2147483649",
        "0.1e2147483647",
        "0.1e-2147483647",
        "100000000000000000000e-2147483647",
        "100000000000000000000e2147483647",
        "1e0000000001",
        "18446744073709551614",
        "18446744073709551615",
        "18446744073709551616",
        "18446744073709551617",
        "184467440737095516150",
        "18446744073709551615.0",
        "18446744073709551615.5",
        "18446744073709551616.",
        "18446744073709551616.5",
        "18446744073709551616e0",
        "18446744073709551616e",
        "1.8446744073709551616",
        "1.84467440737095516159",
        "0.18446744073709551616",
        "0.184467440737095516159e5",
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775807",
        "-9223372036854775808",
        "-9223372036854775809",
        "-18446744073709551615",
        "-18446744073709551616",
        "-9223372036854775808.0",
        "9007199254740993",
        "9007199254740993.0",
        "0.1",
        "0.3",
        "1.1",
        "1e23",
        "8.5e22",
        "123456789.123456789e-5",
        "0.000001",
        "0.00000000000000000000000000000000000000000001",
        "1.00000000000000000000000000000000000000000001",
        "1e1 ",
        " 1e1",
        "1e1,",
        "1 1",
        "1\u{0}",
        "1.0\u{0}",
        "1e5\u{0}",
        "0\u{0}",
        "-\u{0}",
        "1.\u{0}",
        "1e\u{0}",
    ];
    let mut accepted = 0;
    for text in texts {
        accepted += u64::from(parse(text.as_bytes()));
        let wrapped = format!("[{text},{text}]");
        parse(wrapped.as_bytes());
    }
    report(
        "float parse, listed numbers",
        texts.len() as u64,
        Some(accepted),
    );
}

/// The reference keeps at most twenty significant digits and applies one
/// multiplication or division, so some texts land one step away from the
/// nearest float. That behaviour is part of the contract.
#[test]
fn rounding_is_not_always_to_nearest() {
    let mut rng = Rng::new(0x0ff);
    let mut off = 0;
    for _ in 0..200_000 {
        let f = finite(&mut rng);
        let text = serde_json::to_string(&f).unwrap();
        let parsed = alt_json::parse(text.as_bytes()).unwrap().as_f64().unwrap();
        let reference: f64 = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.to_bits(), reference.to_bits());
        off += u64::from(parsed.to_bits() != f.to_bits());
    }
    println!(
        "float parse: {off} of 200000 shortest texts do not read back to the same float, on both sides"
    );
}
