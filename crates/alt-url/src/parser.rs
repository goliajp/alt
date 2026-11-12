use crate::host::HostKind;
use crate::percent::{self, EncodeSet};
use crate::{ParseError, Url};

mod authority;
mod file;
mod path;

pub(crate) fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

fn is_special(scheme: &str) -> bool {
    scheme == "file" || default_port(scheme).is_some()
}

/// The length of the scheme at the start of `input`, if a `:` ends one.
fn scheme_length(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    if !bytes.first()?.is_ascii_alphabetic() {
        return None;
    }
    let length = bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')))?;
    (bytes[length] == b':').then_some(length)
}

pub(crate) fn parse(input: &str, base: Option<&Url>) -> Result<Url, ParseError> {
    let bytes = input.as_bytes();
    let start = bytes.iter().position(|&b| b > b' ').unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| b > b' ')
        .map_or(start, |i| i + 1);
    let input = &input[start..end];
    if input.bytes().any(|b| matches!(b, b'\t' | b'\n' | b'\r')) {
        let cleaned: String = input
            .chars()
            .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
            .collect();
        run(&cleaned, base)
    } else {
        run(input, base)
    }
}

fn run(input: &str, base: Option<&Url>) -> Result<Url, ParseError> {
    let capacity = input.len() + 4 + base.map_or(0, |base| base.as_str().len());
    let mut parser = Parser::new(String::with_capacity(capacity));
    let rest = match scheme_length(input) {
        Some(length) => parser.absolute(input, length, base)?,
        None => {
            let base = base.ok_or(ParseError::RelativeUrlWithoutBase)?;
            if base.cannot_be_a_base() {
                if !input.starts_with('#') {
                    return Err(ParseError::RelativeUrlWithCannotBeABaseBase);
                }
                parser.copy_without_fragment(base);
                input
            } else {
                parser.out.push_str(&base.as_str()[..=base.scheme_end]);
                parser.set_scheme(base.scheme_end);
                if parser.file {
                    parser.file(input, Some(base))?
                } else {
                    parser.relative(input, base)?
                }
            }
        }
    };
    Ok(parser.finish(rest))
}

pub(crate) struct Parser {
    out: String,
    special: bool,
    file: bool,
    authority: bool,
    scheme_end: usize,
    username_end: usize,
    host_start: usize,
    host_end: usize,
    host: HostKind,
    port: Option<u16>,
    path_start: usize,
    query_start: Option<usize>,
}

impl Parser {
    fn new(out: String) -> Self {
        Self {
            out,
            special: false,
            file: false,
            authority: false,
            scheme_end: 0,
            username_end: 0,
            host_start: 0,
            host_end: 0,
            host: HostKind::None,
            port: None,
            path_start: 0,
            query_start: None,
        }
    }

    /// Records the scheme already written to `out`, with its `:`.
    fn set_scheme(&mut self, length: usize) {
        let scheme = &self.out[..length];
        self.file = scheme == "file";
        self.special = is_special(scheme);
        self.scheme_end = length;
        self.username_end = length + 1;
        self.host_start = length + 1;
        self.host_end = length + 1;
        self.path_start = length + 1;
    }

    fn absolute<'i>(
        &mut self,
        input: &'i str,
        length: usize,
        base: Option<&Url>,
    ) -> Result<&'i str, ParseError> {
        self.out.push_str(&input[..=length]);
        self.out.make_ascii_lowercase();
        self.set_scheme(length);
        let rest = &input[length + 1..];
        if self.file {
            return self.file(rest, base.filter(|base| base.scheme() == "file"));
        }
        if self.special {
            return match base.filter(|base| base.scheme() == &self.out[..length]) {
                Some(base) => self.relative(rest, base),
                None => self.authority(rest.trim_start_matches(['/', '\\'])),
            };
        }
        if let Some(rest) = rest.strip_prefix("//") {
            self.authority(rest)
        } else if let Some(rest) = rest.strip_prefix('/') {
            Ok(self.path(rest))
        } else {
            Ok(self.opaque_path(rest))
        }
    }

    fn is_slash(&self, byte: u8) -> bool {
        byte == b'/' || (self.special && byte == b'\\')
    }

    /// Resolves an input without an authority of its own against `base`.
    fn relative<'i>(&mut self, input: &'i str, base: &Url) -> Result<&'i str, ParseError> {
        let bytes = input.as_bytes();
        if bytes.first().is_some_and(|&b| self.is_slash(b)) {
            if self.special && bytes.get(1).is_some_and(|&b| self.is_slash(b)) {
                return self.authority(input.trim_start_matches(['/', '\\']));
            }
            if let Some(rest) = input.strip_prefix("//") {
                return self.authority(rest);
            }
            self.copy_authority(base);
            return Ok(self.path(&input[1..]));
        }
        self.copy_authority(base);
        self.out.push_str(base.path());
        match bytes.first() {
            None | Some(b'#') => {
                self.end_path();
                self.copy_query(base);
            }
            Some(b'?') => self.end_path(),
            Some(_) => {
                self.shorten_path();
                return Ok(self.path(input));
            }
        }
        Ok(input)
    }

    /// Replaces `out` with the scheme and authority of `base`.
    fn copy_authority(&mut self, base: &Url) {
        self.out.clear();
        self.authority = base.has_authority();
        let end = if self.authority {
            base.path_start
        } else {
            base.scheme_end + 1
        };
        self.out.push_str(&base.as_str()[..end]);
        self.username_end = base.username_end;
        self.host_start = base.host_start;
        self.host_end = base.host_end;
        self.host = base.host;
        self.port = base.port;
        self.path_start = end;
    }

    fn copy_query(&mut self, base: &Url) {
        if let Some(query) = base.query() {
            self.query_start = Some(self.out.len());
            self.out.push('?');
            self.out.push_str(query);
        }
    }

    fn copy_without_fragment(&mut self, base: &Url) {
        let end = base.fragment_start.unwrap_or(base.as_str().len());
        self.out.push_str(&base.as_str()[..end]);
        self.set_scheme(base.scheme_end);
        self.path_start = base.path_start;
        self.query_start = base.query_start;
    }

    fn opaque_path<'i>(&mut self, input: &'i str) -> &'i str {
        let end = input.find(['?', '#']).unwrap_or(input.len());
        let (path, rest) = input.split_at(end);
        match path.strip_suffix(' ').filter(|_| !rest.is_empty()) {
            Some(path) => {
                percent::encode(&mut self.out, path, EncodeSet::C0_CONTROL);
                self.out.push_str("%20");
            }
            None => percent::encode(&mut self.out, path, EncodeSet::C0_CONTROL),
        }
        rest
    }

    /// Appends the query and fragment in `rest`, which is empty or starts at `?` or `#`.
    fn finish(mut self, mut rest: &str) -> Url {
        if let Some(query) = rest.strip_prefix('?') {
            let end = query.find('#').unwrap_or(query.len());
            let set = if self.special {
                EncodeSet::SPECIAL_QUERY
            } else {
                EncodeSet::QUERY
            };
            self.query_start = Some(self.out.len());
            self.out.push('?');
            percent::encode(&mut self.out, &query[..end], set);
            rest = &query[end..];
        }
        let mut fragment_start = None;
        if let Some(fragment) = rest.strip_prefix('#') {
            fragment_start = Some(self.out.len());
            self.out.push('#');
            percent::encode(&mut self.out, fragment, EncodeSet::FRAGMENT);
        }
        Url {
            serialization: self.out,
            scheme_end: self.scheme_end,
            username_end: self.username_end,
            host_start: self.host_start,
            host_end: self.host_end,
            host: self.host,
            port: self.port,
            path_start: self.path_start,
            query_start: self.query_start,
            fragment_start,
        }
    }
}
