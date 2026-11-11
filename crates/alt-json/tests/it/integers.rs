use crate::check::{output, parse, report};
use crate::rng::Rng;
use alt_json::Value;

fn check_u64(n: u64, cases: &mut u64) {
    let text = output(&Value::from(n));
    assert_eq!(text, n.to_string().as_bytes());
    assert!(parse(&text));
    assert!(parse(format!("-{n}").as_bytes()));
    *cases += 1;
}

fn check_i64(n: i64, cases: &mut u64) {
    let text = output(&Value::from(n));
    assert_eq!(text, n.to_string().as_bytes());
    assert!(parse(&text));
    *cases += 1;
}

#[test]
fn boundaries() {
    let mut cases = 0;
    for power in 0..=19 {
        let ten = 10u64.pow(power);
        for n in [
            ten - 1,
            ten,
            ten + 1,
            ten.wrapping_mul(5),
            ten.wrapping_mul(9).wrapping_add(9),
        ] {
            check_u64(n, &mut cases);
            check_i64(n as i64, &mut cases);
            check_i64((n as i64).wrapping_neg(), &mut cases);
        }
    }
    for bit in 0..64 {
        let two = 1u64 << bit;
        for n in [two - 1, two, two + 1, !two] {
            check_u64(n, &mut cases);
            check_i64(n as i64, &mut cases);
            check_i64((n as i64).wrapping_neg(), &mut cases);
        }
    }
    for n in [
        0,
        1,
        u64::MAX,
        u64::MAX - 1,
        i64::MAX as u64,
        i64::MAX as u64 + 1,
    ] {
        check_u64(n, &mut cases);
    }
    for n in [i64::MIN, i64::MIN + 1, -1, 0, i64::MAX] {
        check_i64(n, &mut cases);
    }
    for n in 0..=100_000u64 {
        check_u64(n, &mut cases);
        check_i64(-(n as i64), &mut cases);
    }
    report("integers, boundaries", cases, None);
}

#[test]
fn random() {
    let mut rng = Rng::new(0x147);
    let mut cases = 0;
    for _ in 0..500_000 {
        let n = rng.next() >> rng.below(64);
        check_u64(n, &mut cases);
        check_i64(n as i64, &mut cases);
        check_i64((n as i64).wrapping_neg(), &mut cases);
    }
    report("integers, random", cases, None);
}

/// integers outside both ranges are read as floats
#[test]
fn beyond_the_integer_ranges() {
    let mut rng = Rng::new(0xb16);
    let mut cases = 0;
    for offset in 0..2000u128 {
        for text in [
            (u64::MAX as u128 - 1000 + offset).to_string(),
            format!("-{}", i64::MAX as u128 - 1000 + offset),
            format!("-{}", u64::MAX as u128 - 1000 + offset),
        ] {
            assert!(parse(text.as_bytes()));
            cases += 1;
        }
    }
    for _ in 0..500_000 {
        let mut text = String::new();
        if rng.chance(2) {
            text.push('-');
        }
        text.push((b'1' + rng.below(9) as u8) as char);
        for _ in 0..17 + rng.below(25) {
            text.push((b'0' + rng.below(10) as u8) as char);
        }
        assert!(parse(text.as_bytes()));
        cases += 1;
    }
    report("integers, beyond the integer ranges", cases, None);
}
