use alt_url::{Host, ParseError, Url};
use std::net::{Ipv4Addr, Ipv6Addr};

#[test]
fn typed_hosts() {
    let host = |input: &str| {
        Url::parse(input)
            .unwrap()
            .host()
            .map(|host| host.to_owned())
    };
    assert_eq!(
        host("http://localhost:3000"),
        Some(Host::Domain("localhost".into()))
    );
    assert_eq!(host("http://0x7f.1"), Some(Host::Ipv4(Ipv4Addr::LOCALHOST)));
    assert_eq!(host("http://[0::1]"), Some(Host::Ipv6(Ipv6Addr::LOCALHOST)));
    assert_eq!(
        host("foo://Opaque%41"),
        Some(Host::Domain("Opaque%41".into()))
    );
    assert_eq!(host("file:///etc/hosts"), None);
    assert_eq!(host("mailto:someone@example.com"), None);
}

#[test]
fn ipv6_serialization() {
    for (input, expected) in [
        ("[1:0:0:2:0:0:0:3]", "[1:0:0:2::3]"),
        ("[0:0:1:0:0:1:0:0]", "[::1:0:0:1:0:0]"),
        ("[1:2:3:4:5:6:7:0]", "[1:2:3:4:5:6:7:0]"),
        ("[::FFFF:192.168.0.1]", "[::ffff:c0a8:1]"),
        ("[1:2:3:4:5:6:1.2.3.4]", "[1:2:3:4:5:6:102:304]"),
        ("[::]", "[::]"),
    ] {
        let url = Url::parse(&format!("http://{input}/")).unwrap();
        assert_eq!(url.host_str(), Some(expected));
        assert_eq!(url.host().unwrap().to_string(), expected);
    }
}

#[test]
fn errors() {
    for (input, error) in [
        ("https://", ParseError::EmptyHost),
        ("https://user@/", ParseError::EmptyHost),
        ("https://:443/", ParseError::EmptyHost),
        ("https://a:65536/", ParseError::InvalidPort),
        ("https://a:8x/", ParseError::InvalidPort),
        ("https://1.2.3.256/", ParseError::InvalidIpv4Address),
        ("https://[1::2::3]/", ParseError::InvalidIpv6Address),
        ("https://[::1/", ParseError::InvalidIpv6Address),
        ("https://a b/", ParseError::InvalidDomainCharacter),
        ("https://a%25b/", ParseError::InvalidDomainCharacter),
        ("/path", ParseError::RelativeUrlWithoutBase),
    ] {
        assert_eq!(Url::parse(input), Err(error), "{input}");
    }
    let opaque = Url::parse("mailto:someone@example.com").unwrap();
    assert_eq!(
        opaque.join("x"),
        Err(ParseError::RelativeUrlWithCannotBeABaseBase)
    );
    assert_eq!(
        opaque.join("#top").unwrap().as_str(),
        "mailto:someone@example.com#top"
    );
}

#[test]
fn join() {
    let base = Url::parse("https://user:pw@example.com:8443/a/b/c?q=1#f").unwrap();
    for (input, expected) in [
        ("", "https://user:pw@example.com:8443/a/b/c?q=1"),
        ("d", "https://user:pw@example.com:8443/a/b/d"),
        ("../d?x", "https://user:pw@example.com:8443/a/d?x"),
        ("/d/./e/../f", "https://user:pw@example.com:8443/d/f"),
        ("?y", "https://user:pw@example.com:8443/a/b/c?y"),
        ("#g", "https://user:pw@example.com:8443/a/b/c?q=1#g"),
        ("//other.example/x", "https://other.example/x"),
        ("https:d", "https://user:pw@example.com:8443/a/b/d"),
        ("http://127.0.0.1:80/", "http://127.0.0.1/"),
    ] {
        let url = base.join(input).unwrap();
        assert_eq!(url.as_str(), expected, "{input:?}");
        assert_eq!(Url::parse(expected).unwrap(), url, "{input:?}");
    }
}
