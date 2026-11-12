//! Rows of the Unicode IDNA conformance data in `tests/data/idna-test-v2.txt`,
//! which states how they were selected, and hosts recorded from the parser
//! this crate replaced.
use alt_url::Url;

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let (digits, after) = match rest[at..].strip_prefix("\\x{") {
            Some(braced) => braced.split_once('}').unwrap(),
            None => rest[at + 2..].split_at(4),
        };
        out.push(char::from_u32(u32::from_str_radix(digits, 16).unwrap()).unwrap());
        rest = after;
    }
    out + rest
}

#[test]
fn conformance_rows() {
    let path = format!("{}/tests/data/idna-test-v2.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    let (mut accepted, mut rejected) = (0, 0);
    for row in text.lines().filter(|row| !row.starts_with('#')) {
        let (source, expected) = row.split_once("; ").unwrap();
        let source = unescape(source);
        // a valid left-to-right label in front sends an ASCII source through full processing
        let prefix = if source.is_ascii() {
            ("ü.", "xn--tda.")
        } else {
            ("", "")
        };
        let parsed = Url::parse(&format!("https://{}{source}/", prefix.0));
        if expected == "!" {
            assert!(parsed.is_err(), "{row}: {parsed:?}");
            rejected += 1;
        } else {
            let expected = format!("{}{expected}", prefix.1);
            let host = parsed.as_ref().ok().and_then(Url::host_str);
            assert_eq!(host, Some(expected.as_str()), "{row}");
            accepted += 1;
        }
    }
    assert_eq!((accepted, rejected), (717, 5181));
    assert_eq!(alt_url::UNICODE_VERSION, "17.0.0");
}

/// recorded from the parser this crate replaced
const RECORDED: &[(&str, &str)] = &[
    (
        "https://\u{4f8b}\u{3048}.jp/",
        "https://xn--r8jz45g.jp/ origin=https://xn--r8jz45g.jp",
    ),
    (
        "https://b\u{fc}cher.example/",
        "https://xn--bcher-kva.example/ origin=https://xn--bcher-kva.example",
    ),
    (
        "https://B\u{fc}cher.Example/",
        "https://xn--bcher-kva.example/ origin=https://xn--bcher-kva.example",
    ),
    (
        "https://\u{43f}\u{440}\u{438}\u{43c}\u{435}\u{440}.\u{440}\u{444}/",
        "https://xn--e1afmkfd.xn--p1ai/ origin=https://xn--e1afmkfd.xn--p1ai",
    ),
    (
        "https://\u{645}\u{62b}\u{627}\u{644}.\u{625}\u{62e}\u{62a}\u{628}\u{627}\u{631}/",
        "https://xn--mgbh0fb.xn--kgbechtv/ origin=https://xn--mgbh0fb.xn--kgbechtv",
    ),
    (
        "https://\u{65e5}\u{672c}\u{8a9e}\u{3002}\u{ff2a}\u{ff30}/",
        "https://xn--wgv71a119e.jp/ origin=https://xn--wgv71a119e.jp",
    ),
    (
        "https://fa\u{df}.de/",
        "https://xn--fa-hia.de/ origin=https://xn--fa-hia.de",
    ),
    (
        "https://xn--bcher-kva.example/",
        "https://xn--bcher-kva.example/ origin=https://xn--bcher-kva.example",
    ),
    (
        "https://%E4%BE%8B%E3%81%88.jp/",
        "https://xn--r8jz45g.jp/ origin=https://xn--r8jz45g.jp",
    ),
    (
        "https://\u{d55c}\u{ad6d}\u{c5b4}.example/",
        "https://xn--3e0bk47br7k.example/ origin=https://xn--3e0bk47br7k.example",
    ),
    (
        "https://e\u{301}cole.fr/",
        "https://xn--cole-9oa.fr/ origin=https://xn--cole-9oa.fr",
    ),
    (
        "https://www.\u{ff45}\u{ff58}\u{ff41}\u{ff4d}\u{ff50}\u{ff4c}\u{ff45}.com/",
        "https://www.example.com/ origin=https://www.example.com",
    ),
    (
        "https://a\u{ad}b.example/",
        "https://ab.example/ origin=https://ab.example",
    ),
    ("https://xn--a.\u{df}/", "error"),
    (
        "https://xn--zca.\u{df}/",
        "https://xn--zca.xn--zca/ origin=https://xn--zca.xn--zca",
    ),
    (
        "https://XN--ZCA.\u{df}/",
        "https://xn--zca.xn--zca/ origin=https://xn--zca.xn--zca",
    ),
    ("https://\u{df}.xn--/", "error"),
    ("https://\u{5d0}.1/", "error"),
    ("https://1\u{5d0}/", "error"),
    ("https://a\u{200d}b/", "error"),
    (
        "https://\u{dc1}\u{dca}\u{200d}\u{dbb}\u{dd3}/",
        "https://xn--10cl1a0b660p/ origin=https://xn--10cl1a0b660p",
    ),
    ("https://\u{301}a.\u{fc}/", "error"),
    (
        "https://\u{fc}.xn--tda/",
        "https://xn--tda.xn--tda/ origin=https://xn--tda.xn--tda",
    ),
    ("https://\u{fc}.xn--u-ccb/", "error"),
    (
        "https://\u{fc}..com/",
        "https://xn--tda..com/ origin=https://xn--tda..com",
    ),
    (
        "https://\u{fc}.-a-.com/",
        "https://xn--tda.-a-.com/ origin=https://xn--tda.-a-.com",
    ),
    ("https://\u{fc}.a\u{fffd}/", "error"),
    ("https://\u{fc}.xn--zn7c/", "error"),
    (
        "https://\u{ff58}\u{ff4e}--\u{ff5a}\u{ff43}\u{ff41}.\u{fc}/",
        "https://xn--zca.xn--tda/ origin=https://xn--zca.xn--tda",
    ),
    (
        "https://\u{fc}.%41%2E/",
        "https://xn--tda.a./ origin=https://xn--tda.a.",
    ),
    ("https://\u{fc}.a%2fb/", "error"),
    ("https://\u{fc}.\u{ff05}/", "error"),
    ("https://\u{fc}.1/", "error"),
    (
        "https://\u{ff11}\u{ff12}\u{ff17}.\u{ff10}.\u{ff10}.\u{ff11}/",
        "https://127.0.0.1/ origin=https://127.0.0.1",
    ),
    (
        "https://\u{ac00}\u{ac01}.\u{fc}/",
        "https://xn--o39ac.xn--tda/ origin=https://xn--o39ac.xn--tda",
    ),
    (
        "https://\u{1100}\u{1161}\u{11a8}.kr/",
        "https://xn--p39a.kr/ origin=https://xn--p39a.kr",
    ),
    (
        "https://\u{1c6}.\u{1c5}.\u{1c4}/",
        "https://xn--d-toa.xn--d-toa.xn--d-toa/ origin=https://xn--d-toa.xn--d-toa.xn--d-toa",
    ),
];

#[test]
fn recorded_hosts() {
    assert_eq!(RECORDED.len(), 37);
    for &(input, expected) in RECORDED {
        let actual = Url::parse(input).map_or("error".to_owned(), |url| {
            format!("{url} origin={}", url.origin().ascii_serialization())
        });
        assert_eq!(actual, expected, "{input:?}");
    }
}

/// a decoded label that itself starts with `xn--` is invalid, though the
/// replaced parser accepted it
#[test]
fn decoded_label_with_the_prefix() {
    assert_eq!(
        Url::parse("https://\u{fc}.xn--xn---epa/"),
        Err(alt_url::ParseError::IdnaError)
    );
}

#[test]
fn ascii_hosts_keep_their_labels() {
    for host in ["xn--a", "xn--", "xn--1ug.example", "XN--R8JZ45G.jp"] {
        let url = Url::parse(&format!("https://{host}/")).unwrap();
        assert_eq!(url.host_str(), Some(host.to_ascii_lowercase().as_str()));
    }
}
