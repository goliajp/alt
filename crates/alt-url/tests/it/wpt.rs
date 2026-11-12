//! The web-platform-tests URL data in `tests/data`. Hosts that are not
//! ASCII need IDNA mapping, which the crate reports as `NonAsciiHost`;
//! those cases are counted instead of compared.
use crate::json::Json;
use alt_url::{ParseError, Url};

fn expected<'a>(case: &'a Json, name: &str) -> &'a str {
    case.get(name).and_then(Json::text).unwrap()
}

fn prefixed(prefix: char, value: Option<&str>) -> String {
    match value {
        Some(value) if !value.is_empty() => format!("{prefix}{value}"),
        _ => String::new(),
    }
}

fn check(case: &Json, url: &Url) {
    let host = match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (host, _) => host.unwrap_or("").to_owned(),
    };
    let port = url.port().map(|port| port.to_string()).unwrap_or_default();
    let actual = [
        ("href", url.as_str().to_owned()),
        ("protocol", format!("{}:", url.scheme())),
        ("username", url.username().to_owned()),
        ("password", url.password().unwrap_or("").to_owned()),
        ("host", host),
        ("hostname", url.host_str().unwrap_or("").to_owned()),
        ("port", port),
        ("pathname", url.path().to_owned()),
        ("search", prefixed('?', url.query())),
        ("hash", prefixed('#', url.fragment())),
    ];
    for (name, actual) in actual {
        assert_eq!(actual, expected(case, name), "{name} of {case:?}");
    }
    if let Some(origin) = case.get("origin").and_then(Json::text) {
        assert_eq!(url.origin().ascii_serialization(), origin, "{case:?}");
    }
}

#[test]
fn urltestdata() {
    let (mut compared, mut non_ascii) = (0, 0);
    for case in Json::load("urltestdata.json").items() {
        let Some(input) = case.get("input").and_then(Json::text) else {
            continue;
        };
        let parsed = match case.get("base").and_then(Json::text) {
            Some(base) => Url::parse(base).and_then(|base| base.join(input)),
            None => Url::parse(input),
        };
        let failure = matches!(case.get("failure"), Some(Json::Bool(true)));
        match parsed {
            Err(ParseError::NonAsciiHost) if !failure => non_ascii += 1,
            Err(_) => assert!(failure, "{case:?}"),
            Ok(url) => {
                assert!(!failure, "{case:?} parsed as {url}");
                check(case, &url);
            }
        }
        compared += 1;
    }
    assert_eq!((compared, non_ascii), (896, 13));
}

/// Each host parsed as `https://{input}/x`
#[test]
fn toascii() {
    let (mut compared, mut non_ascii) = (0, 0);
    for case in Json::load("toascii.json").items() {
        let Some(input) = case.get("input").and_then(Json::text) else {
            continue;
        };
        let output = case.get("output").and_then(Json::text);
        match (Url::parse(&format!("https://{input}/x")), output) {
            (Err(ParseError::NonAsciiHost), _) => non_ascii += 1,
            (Err(_), None) => {}
            (Ok(url), Some(output)) => assert_eq!(url.host_str(), Some(output)),
            (parsed, output) => panic!("{input:?}: {parsed:?}, expected {output:?}"),
        }
        compared += 1;
    }
    assert_eq!((compared, non_ascii), (87, 63));
}

/// UTF-8 encoding of the query and the fragment
#[test]
fn percent_encoding() {
    let mut compared = 0;
    for case in Json::load("percent-encoding.json").items() {
        let Some(input) = case.get("input").and_then(Json::text) else {
            continue;
        };
        let output = case.get("output").and_then(|output| output.get("utf-8"));
        let output = output.and_then(Json::text).unwrap();
        let url = Url::parse(&format!("https://doesnotmatter.invalid/?{input}#{input}")).unwrap();
        assert_eq!(url.query(), Some(output));
        assert_eq!(url.fragment(), Some(output));
        compared += 1;
    }
    assert_eq!(compared, 7);
}
