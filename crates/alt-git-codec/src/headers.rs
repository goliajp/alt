use bstr::{BStr, BString, ByteSlice};

/// The shared text layout of commit and tag objects: a run of
/// `name SP value LF` headers (values may continue on `SP`-prefixed lines),
/// then a blank line, then the message.
///
/// Headers are kept in original order with unknown names preserved verbatim,
/// so `serialize_into(parse(x)) == x` holds for any well-formed object —
/// the foundation of round-trip fidelity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderBlock {
    pub headers: Vec<(BString, BString)>,
    /// Whether the blank separator line was present (it is in all real
    /// objects, but round-tripping must not invent one).
    pub has_separator: bool,
    pub message: BString,
}

impl HeaderBlock {
    pub fn parse(data: &[u8]) -> Result<Self, ObjectParseError> {
        let mut headers: Vec<(BString, BString)> = Vec::new();
        let mut rest = data;
        loop {
            if rest.is_empty() {
                return Ok(Self {
                    headers,
                    has_separator: false,
                    message: BString::default(),
                });
            }
            if rest[0] == b'\n' {
                return Ok(Self {
                    headers,
                    has_separator: true,
                    message: rest[1..].into(),
                });
            }
            let line_end = rest
                .find_byte(b'\n')
                .ok_or(ObjectParseError::Headers("unterminated header line"))?;
            let line = &rest[..line_end];
            rest = &rest[line_end + 1..];
            if line[0] == b' ' {
                let Some(last) = headers.last_mut() else {
                    return Err(ObjectParseError::Headers("continuation before any header"));
                };
                last.1.push(b'\n');
                last.1.extend_from_slice(&line[1..]);
            } else {
                let sp = line
                    .find_byte(b' ')
                    .ok_or(ObjectParseError::Headers("header line without space"))?;
                headers.push((line[..sp].into(), line[sp + 1..].into()));
            }
        }
    }

    pub fn serialize_into(&self, out: &mut Vec<u8>) {
        for (name, value) in &self.headers {
            out.extend_from_slice(name);
            out.push(b' ');
            let mut lines = value.split(|&b| b == b'\n');
            out.extend_from_slice(lines.next().unwrap());
            for continuation in lines {
                out.push(b'\n');
                out.push(b' ');
                out.extend_from_slice(continuation);
            }
            out.push(b'\n');
        }
        if self.has_separator {
            out.push(b'\n');
            out.extend_from_slice(&self.message);
        }
    }

    /// First value of header `name`, if present.
    pub fn value(&self, name: &[u8]) -> Option<&BStr> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_bstr())
    }

    /// All values of header `name`, in order.
    pub fn values<'a>(&'a self, name: &'a [u8]) -> impl Iterator<Item = &'a BStr> {
        self.headers
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_bstr())
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ObjectParseError {
    #[error("header block: {0}")]
    Headers(&'static str),
    #[error("tree: {0}")]
    Tree(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
        parent e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n\
        author A U Thor <a@example.com> 1700000000 +0900\n\
        committer A U Thor <a@example.com> 1700000001 +0900\n\
        gpgsig -----BEGIN PGP SIGNATURE-----\n\
        \x20sig line two\n\
        \x20-----END PGP SIGNATURE-----\n\
        \n\
        subject line\n\
        \n\
        body, not a header: x y\n";

    #[test]
    fn parses_and_round_trips() {
        let block = HeaderBlock::parse(SAMPLE).unwrap();
        assert_eq!(block.headers.len(), 5);
        assert!(block.has_separator);
        assert_eq!(
            block.value(b"gpgsig").unwrap().as_ref() as &[u8],
            b"-----BEGIN PGP SIGNATURE-----\nsig line two\n-----END PGP SIGNATURE-----"
        );
        let mut out = Vec::new();
        block.serialize_into(&mut out);
        assert_eq!(out, SAMPLE);
    }

    #[test]
    fn round_trips_without_separator_or_message() {
        let data = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n";
        let block = HeaderBlock::parse(data).unwrap();
        assert!(!block.has_separator);
        let mut out = Vec::new();
        block.serialize_into(&mut out);
        assert_eq!(out, data);
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(HeaderBlock::parse(b" lonely continuation\n").is_err());
        assert!(HeaderBlock::parse(b"nospace\n").is_err());
        assert!(HeaderBlock::parse(b"tree abc").is_err());
    }
}
