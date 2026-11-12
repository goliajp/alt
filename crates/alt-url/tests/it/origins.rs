//! Expected values recorded from the parser this crate replaced, for the
//! kinds of input the callers pass: configured origins and redirect targets.
use alt_url::{Host, Url};

fn summary(url: &Url) -> String {
    let host = match url.host() {
        None => "none".to_owned(),
        Some(Host::Domain(domain)) => format!("domain:{domain}"),
        Some(Host::Ipv4(address)) => format!("ipv4:{address}"),
        Some(Host::Ipv6(address)) => format!("ipv6:{address}"),
    };
    format!(
        "{} origin={} user={} pass={:?} host={} port={:?} path={} query={:?} fragment={:?}",
        url.as_str(),
        url.origin().ascii_serialization(),
        url.username(),
        url.password(),
        host,
        url.port(),
        url.path(),
        url.query(),
        url.fragment()
    )
}

const RECORDED: &[(&str, &str)] = &[
    (
        "https://alt.golia.jp",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://alt.golia.jp/",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://ALT.Golia.JP",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://alt.golia.jp:443",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://alt.golia.jp:8443",
        "https://alt.golia.jp:8443/ origin=https://alt.golia.jp:8443 user= pass=None host=domain:alt.golia.jp port=Some(8443) path=/ query=None fragment=None",
    ),
    (
        "https://alt.golia.jp:08443",
        "https://alt.golia.jp:8443/ origin=https://alt.golia.jp:8443 user= pass=None host=domain:alt.golia.jp port=Some(8443) path=/ query=None fragment=None",
    ),
    (
        "https://alt.golia.jp:",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    ("https://alt.golia.jp:65536", "error"),
    ("https://alt.golia.jp:8x", "error"),
    (
        "https://alt.golia.jp.",
        "https://alt.golia.jp./ origin=https://alt.golia.jp. user= pass=None host=domain:alt.golia.jp. port=None path=/ query=None fragment=None",
    ),
    (
        "http://localhost:3000",
        "http://localhost:3000/ origin=http://localhost:3000 user= pass=None host=domain:localhost port=Some(3000) path=/ query=None fragment=None",
    ),
    (
        "http://LOCALHOST",
        "http://localhost/ origin=http://localhost user= pass=None host=domain:localhost port=None path=/ query=None fragment=None",
    ),
    (
        "http://localhost:80",
        "http://localhost/ origin=http://localhost user= pass=None host=domain:localhost port=None path=/ query=None fragment=None",
    ),
    (
        "http://127.0.0.1:8080",
        "http://127.0.0.1:8080/ origin=http://127.0.0.1:8080 user= pass=None host=ipv4:127.0.0.1 port=Some(8080) path=/ query=None fragment=None",
    ),
    (
        "http://127.1",
        "http://127.0.0.1/ origin=http://127.0.0.1 user= pass=None host=ipv4:127.0.0.1 port=None path=/ query=None fragment=None",
    ),
    (
        "http://0x7f.0.0.1",
        "http://127.0.0.1/ origin=http://127.0.0.1 user= pass=None host=ipv4:127.0.0.1 port=None path=/ query=None fragment=None",
    ),
    (
        "http://2130706433",
        "http://127.0.0.1/ origin=http://127.0.0.1 user= pass=None host=ipv4:127.0.0.1 port=None path=/ query=None fragment=None",
    ),
    ("http://127.0.0.256", "error"),
    ("http://1.2.3.4.5", "error"),
    (
        "http://[::1]:8080",
        "http://[::1]:8080/ origin=http://[::1]:8080 user= pass=None host=ipv6:::1 port=Some(8080) path=/ query=None fragment=None",
    ),
    (
        "http://[0:0:0:0:0:0:0:1]",
        "http://[::1]/ origin=http://[::1] user= pass=None host=ipv6:::1 port=None path=/ query=None fragment=None",
    ),
    (
        "http://[::ffff:127.0.0.1]",
        "http://[::ffff:7f00:1]/ origin=http://[::ffff:7f00:1] user= pass=None host=ipv6:::ffff:127.0.0.1 port=None path=/ query=None fragment=None",
    ),
    ("http://[::1", "error"),
    ("http://[1::2::3]", "error"),
    (
        "http://[2001:DB8::A:0:0:1]:1",
        "http://[2001:db8::a:0:0:1]:1/ origin=http://[2001:db8::a:0:0:1]:1 user= pass=None host=ipv6:2001:db8::a:0:0:1 port=Some(1) path=/ query=None fragment=None",
    ),
    (
        "http://10.0.0.5",
        "http://10.0.0.5/ origin=http://10.0.0.5 user= pass=None host=ipv4:10.0.0.5 port=None path=/ query=None fragment=None",
    ),
    (
        "https://user@alt.golia.jp",
        "https://user@alt.golia.jp/ origin=https://alt.golia.jp user=user pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://user:pw@alt.golia.jp",
        "https://user:pw@alt.golia.jp/ origin=https://alt.golia.jp user=user pass=Some(\"pw\") host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://:pw@alt.golia.jp",
        "https://:pw@alt.golia.jp/ origin=https://alt.golia.jp user= pass=Some(\"pw\") host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://@alt.golia.jp",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://alt.golia.jp/?",
        "https://alt.golia.jp/? origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=Some(\"\") fragment=None",
    ),
    (
        "https://alt.golia.jp?x=1",
        "https://alt.golia.jp/?x=1 origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=Some(\"x=1\") fragment=None",
    ),
    (
        "https://alt.golia.jp/#",
        "https://alt.golia.jp/# origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=Some(\"\")",
    ),
    (
        "https://alt.golia.jp#f",
        "https://alt.golia.jp/#f origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=Some(\"f\")",
    ),
    (
        "https://alt.golia.jp/path",
        "https://alt.golia.jp/path origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/path query=None fragment=None",
    ),
    (
        "https://alt.golia.jp//",
        "https://alt.golia.jp// origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=// query=None fragment=None",
    ),
    (
        "https:alt.golia.jp",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https:/alt.golia.jp",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https:\\\\alt.golia.jp\\x",
        "https://alt.golia.jp/x origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/x query=None fragment=None",
    ),
    (
        "  https://alt.golia.jp  ",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://alt.gol\tia.jp",
        "https://alt.golia.jp/ origin=https://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    ("https://", "error"),
    ("https:///", "error"),
    ("https://:443", "error"),
    ("alt.golia.jp", "error"),
    ("//alt.golia.jp", "error"),
    ("", "error"),
    (
        "ftp://alt.golia.jp",
        "ftp://alt.golia.jp/ origin=ftp://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "ws://alt.golia.jp:80",
        "ws://alt.golia.jp/ origin=ws://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "wss://alt.golia.jp",
        "wss://alt.golia.jp/ origin=wss://alt.golia.jp user= pass=None host=domain:alt.golia.jp port=None path=/ query=None fragment=None",
    ),
    (
        "file:///etc/hosts",
        "file:///etc/hosts origin=null user= pass=None host=none port=None path=/etc/hosts query=None fragment=None",
    ),
    (
        "data:text/plain,hi",
        "data:text/plain,hi origin=null user= pass=None host=none port=None path=text/plain,hi query=None fragment=None",
    ),
    (
        "https://xn--r8jz45g.jp",
        "https://xn--r8jz45g.jp/ origin=https://xn--r8jz45g.jp user= pass=None host=domain:xn--r8jz45g.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://XN--R8JZ45G.jp",
        "https://xn--r8jz45g.jp/ origin=https://xn--r8jz45g.jp user= pass=None host=domain:xn--r8jz45g.jp port=None path=/ query=None fragment=None",
    ),
    (
        "https://a_b.example",
        "https://a_b.example/ origin=https://a_b.example user= pass=None host=domain:a_b.example port=None path=/ query=None fragment=None",
    ),
    ("https://a b.example", "error"),
    (
        "https://a%41b.example",
        "https://aab.example/ origin=https://aab.example user= pass=None host=domain:aab.example port=None path=/ query=None fragment=None",
    ),
    ("https://a%2fb.example", "error"),
    ("https://exa<mple.com", "error"),
    (
        "https://control.example.com/cloud/access",
        "https://control.example.com/cloud/access origin=https://control.example.com user= pass=None host=domain:control.example.com port=None path=/cloud/access query=None fragment=None",
    ),
    (
        "https://control.example.com:8443/cloud/access",
        "https://control.example.com:8443/cloud/access origin=https://control.example.com:8443 user= pass=None host=domain:control.example.com port=Some(8443) path=/cloud/access query=None fragment=None",
    ),
    (
        "http://127.0.0.1:41233/cloud/access",
        "http://127.0.0.1:41233/cloud/access origin=http://127.0.0.1:41233 user= pass=None host=ipv4:127.0.0.1 port=Some(41233) path=/cloud/access query=None fragment=None",
    ),
    (
        "https://control.example.com/cloud/access?installationId=inst_1&challenge=abc-_&state=ff#frag",
        "https://control.example.com/cloud/access?installationId=inst_1&challenge=abc-_&state=ff#frag origin=https://control.example.com user= pass=None host=domain:control.example.com port=None path=/cloud/access query=Some(\"installationId=inst_1&challenge=abc-_&state=ff\") fragment=Some(\"frag\")",
    ),
    (
        "https://example.org/a%20b/c d/../e/./f?q=1 2'\"<>#f g`",
        "https://example.org/a%20b/e/f?q=1%202%27%22%3C%3E#f%20g%60 origin=https://example.org user= pass=None host=domain:example.org port=None path=/a%20b/e/f query=Some(\"q=1%202%27%22%3C%3E\") fragment=Some(\"f%20g%60\")",
    ),
];

#[test]
fn recorded_urls() {
    assert_eq!(RECORDED.len(), 64);
    for &(input, expected) in RECORDED {
        let actual = Url::parse(input).map_or("error".to_owned(), |url| summary(&url));
        assert_eq!(actual, expected, "{input:?}");
    }
}

type Appended = (
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
);

const APPENDED: &[Appended] = &[
    (
        "https://control.example.com/cloud/access",
        &[
            ("installationId", "inst 1&x"),
            ("challenge", "a+b/c=~*\u{e9}'()!"),
            ("state", ""),
        ],
        "https://control.example.com/cloud/access?installationId=inst+1%26x&challenge=a%2Bb%2Fc%3D%7E*%C3%A9%27%28%29%21&state=",
    ),
    (
        "https://c.example/p?",
        &[("a", "b")],
        "https://c.example/p?a=b",
    ),
    (
        "https://c.example/p?x=1#frag",
        &[("a b", "c%d")],
        "https://c.example/p?x=1&a+b=c%25d#frag",
    ),
    (
        "https://c.example/p#frag",
        &[("", "")],
        "https://c.example/p?=#frag",
    ),
];

#[test]
fn appended_query_pairs() {
    for &(base, pairs, expected) in APPENDED {
        let mut url = Url::parse(base).unwrap();
        for (name, value) in pairs {
            url.append_query_pair(name, value);
        }
        assert_eq!(url.as_str(), expected);
        assert_eq!(Url::parse(expected).unwrap(), url);
        let decoded: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let tail = &decoded[decoded.len() - pairs.len()..];
        for ((name, value), pair) in tail.iter().zip(pairs) {
            assert_eq!((name.as_str(), value.as_str()), *pair);
        }
    }
}
