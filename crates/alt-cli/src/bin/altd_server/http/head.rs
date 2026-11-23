//! The head of a request: its line and its headers, and how its body is
//! framed.

use std::io;

use super::paced::Source;

/// the most bytes the head of a request has
const HEAD_BYTES: usize = 64 * 1024;
/// the most headers a request has
const HEADERS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
    Options,
    NonStandard(String),
}

/// The name of a header, compared without regard to case.
#[derive(Debug, Clone)]
pub(crate) struct Field(String);

impl Field {
    pub(crate) fn equiv(&self, name: &str) -> bool {
        self.0.eq_ignore_ascii_case(name)
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Header {
    pub(crate) field: Field,
    pub(crate) value: String,
}

impl Header {
    /// A header, or nothing where the name is no token or the value has a
    /// control character in it.
    pub(crate) fn from_bytes(name: &[u8], value: &[u8]) -> Result<Header, ()> {
        let token = |b: &u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b);
        let plain = |b: &u8| *b == b'\t' || (*b >= 0x20 && *b != 0x7f);
        if name.is_empty() || !name.iter().all(token) || !value.iter().all(plain) {
            return Err(());
        }
        let text = |bytes: &[u8]| String::from_utf8(bytes.to_vec()).map_err(|_| ());
        Ok(Header {
            field: Field(text(name)?),
            value: text(value)?.trim_matches([' ', '\t']).to_owned(),
        })
    }
}

/// How the body of a request ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Framing {
    /// after this many bytes
    Length(u64),
    /// after a chunk of no bytes
    Chunked,
}

pub(super) struct Head {
    pub(super) method: Method,
    pub(super) url: String,
    pub(super) headers: Vec<Header>,
    pub(super) framing: Framing,
    /// the connection is not used for another request
    pub(super) last: bool,
    /// the peer waits to be told to send the body
    pub(super) expects: bool,
}

fn bad(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

impl Head {
    /// The first of `name` among the headers.
    pub(super) fn header(headers: &[Header], name: &str) -> Option<String> {
        let found = headers.iter().find(|h| h.field.equiv(name));
        found.map(|h| h.value.clone())
    }

    /// Reads the head of the next request. Nothing where the peer ended
    /// the connection between requests.
    pub(super) fn read(source: &mut Source) -> io::Result<Option<Head>> {
        source.begin_head();
        let end = loop {
            let end = source.ahead().windows(4).position(|w| w == b"\r\n\r\n");
            if end.unwrap_or(source.ahead().len()) > HEAD_BYTES {
                return Err(bad("the head of the request is too long"));
            }
            if let Some(end) = end {
                break end;
            }
            // a connection that ends or stays silent between two requests
            // is one that is over, and is not answered
            let silent = source.ahead().is_empty();
            match source.more() {
                Ok(0) if silent => return Ok(None),
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Err(error) if silent && error.kind() == io::ErrorKind::TimedOut => return Ok(None),
                more => more?,
            };
        };
        let head = Head::parse(&source.ahead()[..end]);
        source.take(end + 4);
        head.map(Some)
    }

    fn parse(text: &[u8]) -> io::Result<Head> {
        let mut lines = text.split(|b| *b == b'\n').map(|line| match line {
            [line @ .., b'\r'] => line,
            line => line,
        });
        let line = lines.next().unwrap_or_default();
        let mut words = line.split(|b| *b == b' ');
        let (method, url, version) = (words.next(), words.next(), words.next());
        let (Some(method), Some(url), Some(version), None) = (method, url, version, words.next())
        else {
            return Err(bad("the request line"));
        };
        let old = match version {
            b"HTTP/1.1" => false,
            b"HTTP/1.0" => true,
            _ => return Err(bad("the version of the request")),
        };
        let url = String::from_utf8(url.to_vec()).map_err(|_| bad("the target"))?;
        if !url.starts_with('/') || url.bytes().any(|b| b <= 0x20 || b == 0x7f) {
            return Err(bad("the target of the request"));
        }
        let method = match method {
            b"GET" => Method::Get,
            b"HEAD" => Method::Head,
            b"POST" => Method::Post,
            b"PUT" => Method::Put,
            b"DELETE" => Method::Delete,
            b"OPTIONS" => Method::Options,
            other if !other.is_empty() && other.iter().all(u8::is_ascii_uppercase) => {
                Method::NonStandard(String::from_utf8_lossy(other).into_owned())
            }
            _ => return Err(bad("the method of the request")),
        };
        let mut headers = Vec::new();
        for line in lines {
            let colon = line.iter().position(|b| *b == b':');
            let colon = colon.ok_or(bad("a header without a colon"))?;
            let header = Header::from_bytes(&line[..colon], &line[colon + 1..]);
            headers.push(header.map_err(|()| bad("a header"))?);
            if headers.len() > HEADERS {
                return Err(bad("too many headers"));
            }
        }
        let count = |name: &str| headers.iter().filter(|h| h.field.equiv(name)).count();
        let framing = match (count("Transfer-Encoding"), count("Content-Length")) {
            (0, 0) => Framing::Length(0),
            (0, 1) => {
                let length = Head::header(&headers, "Content-Length").unwrap_or_default();
                let digits = !length.is_empty() && length.bytes().all(|b| b.is_ascii_digit());
                let length = length.parse().ok().filter(|_| digits);
                Framing::Length(length.ok_or(bad("the length of the body"))?)
            }
            (1, 0) => match Head::header(&headers, "Transfer-Encoding") {
                Some(coding) if coding.eq_ignore_ascii_case("chunked") => Framing::Chunked,
                _ => return Err(bad("the coding of the body")),
            },
            // a body framed twice is read one way here and another way elsewhere
            _ => return Err(bad("the framing of the body")),
        };
        let connection = Head::header(&headers, "Connection").unwrap_or_default();
        let said = |word: &str| {
            connection
                .split(',')
                .any(|w| w.trim().eq_ignore_ascii_case(word))
        };
        let expect = Head::header(&headers, "Expect").unwrap_or_default();
        Ok(Head {
            method,
            url,
            framing,
            last: said("close") || (old && !said("keep-alive")),
            expects: expect.eq_ignore_ascii_case("100-continue"),
            headers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> io::Result<Head> {
        Head::parse(text.as_bytes())
    }

    #[test]
    fn a_head_is_its_line_its_headers_and_one_framing_of_its_body() {
        let head = parsed(
            "POST /a/git-upload-pack?x=1 HTTP/1.1\r\nHost: h\r\ncontent-length:  12 \r\nGit-Protocol: version=2",
        )
        .unwrap();
        assert_eq!(head.method, Method::Post);
        assert_eq!(head.url, "/a/git-upload-pack?x=1");
        assert_eq!(head.framing, Framing::Length(12));
        assert!(!head.last && !head.expects);
        assert_eq!(
            Head::header(&head.headers, "git-protocol").unwrap(),
            "version=2"
        );

        let head = parsed("POST / HTTP/1.1\r\nTransfer-Encoding: Chunked\r\nExpect: 100-continue\r\nConnection: Keep-Alive, Close").unwrap();
        assert_eq!(head.framing, Framing::Chunked);
        assert!(head.last && head.expects);
        assert!(parsed("GET / HTTP/1.0").unwrap().last);
        assert!(
            !parsed("GET / HTTP/1.0\r\nConnection: keep-alive")
                .unwrap()
                .last
        );
        assert_eq!(
            parsed("GET / HTTP/1.1").unwrap().framing,
            Framing::Length(0)
        );

        for refused in [
            "GET / HTTP/2",
            "GET  / HTTP/1.1",
            "GET / HTTP/1.1 x",
            "get / HTTP/1.1",
            "GET http://h/ HTTP/1.1",
            "GET /a\tb HTTP/1.1",
            "GET / HTTP/1.1\r\nno colon",
            "GET / HTTP/1.1\r\nbad name: v",
            "GET / HTTP/1.1\r\n folded: v",
            "GET / HTTP/1.1\r\nA: b\x01",
            "POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1",
            "POST / HTTP/1.1\r\nContent-Length: 1\r\nTransfer-Encoding: chunked",
            "POST / HTTP/1.1\r\nContent-Length: +1",
            "POST / HTTP/1.1\r\nContent-Length: 1e3",
            "POST / HTTP/1.1\r\nContent-Length: 99999999999999999999999",
            "POST / HTTP/1.1\r\nTransfer-Encoding: gzip, chunked",
        ] {
            assert!(parsed(refused).is_err(), "{refused:?}");
        }
        let many: String = (0..=HEADERS).map(|n| format!("\r\nH{n}: v")).collect();
        assert!(parsed(&format!("GET / HTTP/1.1{many}")).is_err());
    }
}
