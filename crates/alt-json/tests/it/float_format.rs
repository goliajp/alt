use crate::check::{float_output, report};
use crate::rng::Rng;

const MAX_FRACTION: u64 = (1 << 52) - 1;

/// checks the float and the floats a few steps either side of it
fn with_neighbours(f: f64, cases: &mut u64) {
    let bits = f.to_bits() & !(1 << 63);
    for step in -3i64..=3 {
        let near = f64::from_bits(bits.wrapping_add_signed(step) & !(1 << 63));
        if near.is_finite() {
            float_output(near);
            float_output(-near);
            *cases += 2;
        }
    }
}

fn random_bits(seed: u64, count: u64) -> u64 {
    let mut rng = Rng::new(seed);
    let mut cases = 0;
    while cases < count {
        let f = f64::from_bits(rng.next());
        if f.is_finite() {
            float_output(f);
            cases += 1;
        }
    }
    cases
}

#[test]
fn random_bit_patterns() {
    report(
        "float format, random bit patterns",
        random_bits(0xf10a7, 4_000_000),
        None,
    );
}

#[test]
#[ignore = "300 million patterns on all cores, needs --release"]
fn random_bit_patterns_long() {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as u64;
    let per_thread = 300_000_000 / threads;
    let total: u64 = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| scope.spawn(move || random_bits(0xb16 + t, per_thread)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    });
    report("float format, random bit patterns (long)", total, None);
}

#[test]
fn every_exponent() {
    let mut rng = Rng::new(0xe4b0);
    let mut cases = 0;
    for exponent in 0..0x7ffu64 {
        let edge = [
            0,
            1,
            2,
            3,
            MAX_FRACTION,
            MAX_FRACTION - 1,
            MAX_FRACTION - 2,
            1 << 51,
        ];
        let mut fractions = edge.to_vec();
        fractions.extend((0..200).map(|_| rng.next() & MAX_FRACTION));
        // few set bits: short decimals and exact halves live here
        fractions.extend((0..56).map(|_| (1u64 << rng.below(52)) | (1u64 << rng.below(52))));
        for fraction in fractions {
            let f = f64::from_bits((exponent << 52) | fraction);
            float_output(f);
            float_output(-f);
            cases += 2;
        }
    }
    report("float format, every exponent", cases, None);
}

#[test]
fn subnormals() {
    let mut rng = Rng::new(0x5b);
    let mut cases = 0;
    for fraction in 0..=100_000u64 {
        float_output(f64::from_bits(fraction));
        cases += 1;
    }
    for bit in 0..52 {
        with_neighbours(f64::from_bits(1 << bit), &mut cases);
    }
    for _ in 0..500_000 {
        float_output(f64::from_bits(rng.next() & MAX_FRACTION));
        float_output(f64::from_bits((rng.next() & MAX_FRACTION) >> rng.below(52)));
        cases += 2;
    }
    with_neighbours(f64::MIN_POSITIVE, &mut cases);
    report("float format, subnormals", cases, None);
}

#[test]
fn decimal_boundaries() {
    let mut cases = 0;
    let significands = [
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
        "15",
        "25",
        "35",
        "45",
        "55",
        "65",
        "75",
        "85",
        "95",
        "99",
        "125",
        "999",
        "1234",
        "12345678",
        "9007199254740993",
        "1234567890123456",
        "9999999999999999",
        "99999999999999995",
        "12345678901234567",
        "17976931348623157",
        "22250738585072014",
        "49406564584124654",
    ];
    for exponent in -345..=310 {
        for significand in significands {
            let f: f64 = format!("{significand}e{exponent}").parse().unwrap();
            if f.is_finite() {
                with_neighbours(f, &mut cases);
            }
        }
    }
    report("float format, decimal boundaries", cases, None);
}

#[test]
fn short_decimals() {
    let mut rng = Rng::new(0xdec);
    let mut cases = 0;
    while cases < 1_000_000 {
        let digits = rng.range(1, 17) as u32;
        let significand = rng.below(10u64.pow(digits));
        let f: f64 = format!("{significand}e{}", rng.range(-340, 300))
            .parse()
            .unwrap();
        if f.is_finite() {
            float_output(f);
            cases += 1;
        }
    }
    report("float format, short decimals", cases, None);
}

/// positional notation is used for 1e-5 up to, but not including, 1e16
#[test]
fn notation_thresholds() {
    let mut cases = 0;
    for exponent in [
        -8, -7, -6, -5, -4, -3, -1, 0, 1, 14, 15, 16, 17, 20, 21, 22, 23,
    ] {
        for significand in [
            "1",
            "9",
            "12",
            "99",
            "123456789",
            "9999999999999999",
            "1000000000000001",
        ] {
            let f: f64 = format!("{significand}e{exponent}").parse().unwrap();
            with_neighbours(f, &mut cases);
            let shifted: f64 = format!("0.{significand}e{exponent}").parse().unwrap();
            with_neighbours(shifted, &mut cases);
        }
    }
    for (f, text) in [
        (0.0, "0.0"),
        (-0.0, "-0.0"),
        (1.0, "1.0"),
        (1e15, "1000000000000000.0"),
        (9.999999999999998e15, "9999999999999998.0"),
        (1e16, "1e+16"),
        (1.5e16, "1.5e+16"),
        (1e-5, "0.00001"),
        (9.9e-6, "9.9e-6"),
        (1e-6, "1e-6"),
        (1e100, "1e+100"),
        (1.5e-100, "1.5e-100"),
        (f64::MAX, "1.7976931348623157e+308"),
        (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
        (5e-324, "5e-324"),
        (0.1 + 0.2, "0.30000000000000004"),
    ] {
        float_output(f);
        assert_eq!(
            alt_json::to_string(&alt_json::Value::from(f)).unwrap(),
            text
        );
        cases += 1;
    }
    report("float format, notation thresholds", cases, None);
}

#[test]
fn integers_as_floats() {
    let mut rng = Rng::new(0x1f);
    let mut cases = 0;
    for i in 0..=200_000u64 {
        float_output(i as f64);
        cases += 1;
    }
    for power in 0..=63 {
        with_neighbours((1u64 << power) as f64, &mut cases);
    }
    for power in 0..=22 {
        with_neighbours(10f64.powi(power), &mut cases);
    }
    for _ in 0..500_000 {
        float_output((rng.next() >> rng.below(64)) as f64);
        cases += 1;
    }
    report("float format, integers as floats", cases, None);
}

/// infinities and NaN are not numbers in JSON: both sides write null
#[test]
fn non_finite() {
    for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            alt_json::to_string(&alt_json::Value::from(f)).unwrap(),
            "null"
        );
        assert_eq!(serde_json::to_string(&f).unwrap(), "null");
        assert!(alt_json::Number::from_f64(f).is_none());
    }
}
