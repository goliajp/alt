use alt_url::form;

type Parsed = (&'static str, &'static [(&'static str, &'static str)]);

/// recorded from the parser this crate replaced
const RECORDED: &[Parsed] = &[
    ("ticket=ff&state=ab", &[("ticket", "ff"), ("state", "ab")]),
    ("a=1&&b=x+y%21&c", &[("a", "1"), ("b", "x y!"), ("c", "")]),
    ("a=%zz&b=%4", &[("a", "%zz"), ("b", "%4")]),
    (
        "k=%E4%BE%8B&bad=%ff%fe+",
        &[("k", "\u{4f8b}"), ("bad", "\u{fffd}\u{fffd} ")],
    ),
    ("=v&k=&==", &[("", "v"), ("k", ""), ("", "=")]),
    ("a%3Db=c%26d&e=f=g", &[("a=b", "c&d"), ("e", "f=g")]),
    (
        "client_id=alt+cli&scope=repo%3Aread+repo%3Awrite",
        &[("client_id", "alt cli"), ("scope", "repo:read repo:write")],
    ),
    ("", &[]),
];

/// input and expected pairs of the web-platform-tests `urlencoded-parser.any.js`
const WPT: &[Parsed] = &[
    ("test", &[("test", "")]),
    ("\u{FEFF}test=\u{FEFF}", &[("\u{FEFF}test", "\u{FEFF}")]),
    ("%EF%BB%BFtest=%EF%BB%BF", &[("\u{FEFF}test", "\u{FEFF}")]),
    ("%EF%BF%BF=%EF%BF%BF", &[("\u{FFFF}", "\u{FFFF}")]),
    ("%FE%FF", &[("\u{FFFD}\u{FFFD}", "")]),
    ("%FF%FE", &[("\u{FFFD}\u{FFFD}", "")]),
    (
        "\u{2020}&\u{2020}=x",
        &[("\u{2020}", ""), ("\u{2020}", "x")],
    ),
    ("%C2", &[("\u{FFFD}", "")]),
    ("%C2x", &[("\u{FFFD}x", "")]),
    ("", &[]),
    ("a", &[("a", "")]),
    ("a=b", &[("a", "b")]),
    ("a=", &[("a", "")]),
    ("=b", &[("", "b")]),
    ("&", &[]),
    ("&a", &[("a", "")]),
    ("a&", &[("a", "")]),
    ("a&a", &[("a", ""), ("a", "")]),
    ("a&b&c", &[("a", ""), ("b", ""), ("c", "")]),
    ("a=b&c=d", &[("a", "b"), ("c", "d")]),
    ("a=b&c=d&", &[("a", "b"), ("c", "d")]),
    ("&&&a=b&&&&c=d&", &[("a", "b"), ("c", "d")]),
    ("a=a&a=b&a=c", &[("a", "a"), ("a", "b"), ("a", "c")]),
    ("a==a", &[("a", "=a")]),
    ("a=a+b+c+d", &[("a", "a b c d")]),
    ("%=a", &[("%", "a")]),
    ("%a=a", &[("%a", "a")]),
    ("%a_=a", &[("%a_", "a")]),
    ("%61=a", &[("a", "a")]),
    ("%61+%4d%4D=", &[("a MM", "")]),
    ("id=0&value=%", &[("id", "0"), ("value", "%")]),
    ("b=%2sf%2a", &[("b", "%2sf*")]),
    ("b=%2%2af%2a", &[("b", "%2*f*")]),
    ("b=%%2a", &[("b", "%*")]),
];

#[test]
fn parse() {
    assert_eq!(RECORDED.len() + WPT.len(), 42);
    for &(input, expected) in RECORDED.iter().chain(WPT) {
        let pairs: Vec<(String, String)> = form::parse(input.as_bytes()).into_owned().collect();
        let pairs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(pairs, expected, "{input:?}");
    }
}

#[test]
fn unescaped_input_is_borrowed() {
    let (name, value) = form::parse(b"state=abc").next().unwrap();
    assert!(matches!(name, std::borrow::Cow::Borrowed("state")));
    assert!(matches!(value, std::borrow::Cow::Borrowed("abc")));
}

/// pairs and their serialization from `urlsearchparams-stringifier.any.js`
const SERIALIZED: &[(&str, &str, &str)] = &[
    ("a", "b c", "a=b+c"),
    ("a b", "c", "a+b=c"),
    ("a", "b+c", "a=b%2Bc"),
    ("=", "a", "%3D=a"),
    ("b", "&", "b=%26"),
    ("a", "*-._", "a=*-._"),
    ("a", "b%c", "a=b%25c"),
    ("a", "b\0c", "a=b%00c"),
    ("a", "b\u{1F4A9}c", "a=b%F0%9F%92%A9c"),
    ("a\nb", "c\rd", "a%0Ab=c%0Dd"),
    ("a", "b,c~'()!", "a=b%2Cc%7E%27%28%29%21"),
];

#[test]
fn append_pair() {
    for &(name, value, expected) in SERIALIZED {
        let mut out = String::new();
        form::append_pair(&mut out, name, value);
        assert_eq!(out, expected);
    }
}
