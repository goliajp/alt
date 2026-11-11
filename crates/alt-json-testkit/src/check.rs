use alt_json::Value as Alt;
use serde_json::Value as Serde;

/// Same tree, same number kinds, same float bits.
pub fn same(a: &Alt, s: &Serde) -> bool {
    match (a, s) {
        (Alt::Null, Serde::Null) => true,
        (Alt::Bool(a), Serde::Bool(s)) => a == s,
        (Alt::Number(a), Serde::Number(s)) => {
            a.is_u64() == s.is_u64()
                && a.is_i64() == s.is_i64()
                && a.is_f64() == s.is_f64()
                && a.as_u64() == s.as_u64()
                && a.as_i64() == s.as_i64()
                && a.as_f64().map(f64::to_bits) == s.as_f64().map(f64::to_bits)
        }
        (Alt::String(a), Serde::String(s)) => a == s,
        (Alt::Array(a), Serde::Array(s)) => {
            a.len() == s.len() && a.iter().zip(s).all(|(a, s)| same(a, s))
        }
        (Alt::Object(a), Serde::Object(s)) => {
            a.len() == s.len()
                && a.iter()
                    .zip(s)
                    .all(|((ak, av), (sk, sv))| ak == sk && same(av, sv))
        }
        _ => false,
    }
}

pub fn to_serde(a: &Alt) -> Serde {
    match a {
        Alt::Null => Serde::Null,
        Alt::Bool(b) => Serde::Bool(*b),
        Alt::Number(n) => Serde::Number(if let Some(u) = n.as_u64() {
            u.into()
        } else if let Some(i) = n.as_i64() {
            i.into()
        } else {
            serde_json::Number::from_f64(n.as_f64().unwrap()).unwrap()
        }),
        Alt::String(s) => Serde::String(s.clone()),
        Alt::Array(items) => Serde::Array(items.iter().map(to_serde).collect()),
        Alt::Object(map) => {
            Serde::Object(map.iter().map(|(k, v)| (k.clone(), to_serde(v))).collect())
        }
    }
}

pub fn from_serde(s: &Serde) -> Alt {
    match s {
        Serde::Null => Alt::Null,
        Serde::Bool(b) => Alt::Bool(*b),
        Serde::Number(n) => match (n.as_u64(), n.as_i64()) {
            (Some(u), _) => Alt::from(u),
            (_, Some(i)) => Alt::from(i),
            _ => Alt::from(n.as_f64().unwrap()),
        },
        Serde::String(s) => Alt::String(s.clone()),
        Serde::Array(items) => Alt::Array(items.iter().map(from_serde).collect()),
        Serde::Object(map) => Alt::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), from_serde(v)))
                .collect(),
        ),
    }
}

fn show(input: &[u8]) -> String {
    let head = &input[..input.len().min(300)];
    format!(
        "{:?} ({} bytes)",
        String::from_utf8_lossy(head),
        input.len()
    )
}

/// Parses with both; they must both accept with identical values or both
/// reject with identical messages and positions. Returns whether accepted.
pub fn parse(input: &[u8]) -> bool {
    let alt = alt_json::parse(input);
    let serde = serde_json::from_slice::<Serde>(input);
    match (alt, serde) {
        (Ok(a), Ok(s)) => {
            assert!(
                same(&a, &s),
                "values differ for {}: {a:?} vs {s:?}",
                show(input)
            );
            true
        }
        (Err(a), Err(s)) => {
            assert_eq!(
                a.to_string(),
                s.to_string(),
                "errors differ for {}",
                show(input)
            );
            assert_eq!((a.line(), a.column()), (s.line(), s.column()));
            assert_eq!((a.is_eof(), a.is_syntax()), (s.is_eof(), s.is_syntax()));
            false
        }
        (a, s) => panic!("accept/reject differs for {}: {a:?} vs {s:?}", show(input)),
    }
}

/// Compact and indented output must match byte for byte.
pub fn output(a: &Alt) -> Vec<u8> {
    let s = to_serde(a);
    assert!(same(a, &s));
    let compact = alt_json::to_vec(a).unwrap();
    assert_eq!(
        compact,
        serde_json::to_vec(&s).unwrap(),
        "compact output differs: {}",
        show(&compact)
    );
    let pretty = alt_json::to_vec_pretty(a).unwrap();
    assert_eq!(
        pretty,
        serde_json::to_vec_pretty(&s).unwrap(),
        "pretty output differs: {}",
        show(&pretty)
    );
    compact
}

/// Output of one float, through the same entry points as any other value.
pub fn float_output(f: f64) {
    let alt = alt_json::to_vec(&Alt::from(f)).unwrap();
    let serde = serde_json::to_vec(&f).unwrap();
    assert_eq!(
        alt,
        serde,
        "float {:#018x} differs: {} vs {}",
        f.to_bits(),
        String::from_utf8_lossy(&alt),
        String::from_utf8_lossy(&serde)
    );
}

pub fn report(name: &str, cases: u64, accepted: Option<u64>) {
    match accepted {
        Some(accepted) => println!(
            "{name}: {cases} cases, {accepted} accepted, {} rejected",
            cases - accepted
        ),
        None => println!("{name}: {cases} cases"),
    }
}
