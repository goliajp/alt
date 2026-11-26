use alt_bytes::BString;

use crate::{ConfigError, Entry};

/// Parses one config file's syntax into entries, in order. No include
/// processing here — `include.path` lines come out as plain entries.
pub fn parse_file(data: &[u8]) -> Result<Vec<Entry>, ConfigError> {
    let located = parse_located(data).map_err(|(_, what)| ConfigError::Syntax(what))?;
    Ok(located.into_iter().map(|(_, entry)| entry).collect())
}

/// The entries of a file up to the first that is not read, and the error
/// there, where there is one: what git hands the reader of a file whose
/// errors it is told to keep quiet of before it stops.
pub fn parse_until_wrong(data: &[u8]) -> (Vec<Entry>, Option<ConfigError>) {
    let mut located = Vec::new();
    let wrong = parse_into(data, &mut located).err();
    let entries = located.into_iter().map(|(_, entry)| entry).collect();
    (entries, wrong.map(|(_, what)| ConfigError::Syntax(what)))
}

/// As [`parse_file`]; an entry and an error come with the number of the
/// line they are on.
pub(crate) fn parse_located(data: &[u8]) -> Result<Vec<(usize, Entry)>, (usize, &'static str)> {
    let mut located = Vec::new();
    parse_into(data, &mut located)?;
    Ok(located)
}

fn parse_into(data: &[u8], out: &mut Vec<(usize, Entry)>) -> Result<(), (usize, &'static str)> {
    // as git reads a file: no byte order mark, and a line may end in CR LF
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    let mut text = Vec::with_capacity(data.len());
    for (at, &b) in data.iter().enumerate() {
        if b != b'\r' || data.get(at + 1) != Some(&b'\n') {
            text.push(b);
        }
    }
    let mut parser = Parser {
        data: &text,
        pos: 0,
    };
    parser.run(out).map_err(|error| {
        let ConfigError::Syntax(what) = error else {
            unreachable!("the parser reports syntax only")
        };
        let before = &text[..parser.pos.saturating_sub(1).min(text.len())];
        (1 + before.iter().filter(|&&b| b == b'\n').count(), what)
    })
}

/// The section and the subsection a section header names, `header`
/// reaching from its `[` to its `]`.
pub(crate) fn section_of(header: &[u8]) -> Result<(String, Option<BString>), ConfigError> {
    let mut parser = Parser {
        data: header,
        pos: 0,
    };
    let ctx = parser.parse_section_header()?;
    Ok((ctx.section, ctx.subsection))
}

/// What git's parser skips as blank.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

#[derive(Clone)]
struct SectionCtx {
    section: String,
    subsection: Option<BString>,
}

impl<'a> Parser<'a> {
    fn run(&mut self, out: &mut Vec<(usize, Entry)>) -> Result<(), ConfigError> {
        let mut ctx: Option<SectionCtx> = None;
        let (mut line, mut counted) = (1, 0);
        loop {
            self.skip_ws_comments_and_newlines();
            let Some(byte) = self.peek() else {
                return Ok(());
            };
            if byte == b'[' {
                ctx = Some(self.parse_section_header()?);
            } else {
                // as in git, a variable before any section has no section
                let here = ctx.clone().unwrap_or(SectionCtx {
                    section: String::new(),
                    subsection: None,
                });
                let newline = |b: &&u8| **b == b'\n';
                line += self.data[counted..self.pos].iter().filter(newline).count();
                counted = self.pos;
                let (key, value) = self.parse_variable()?;
                let entry = Entry {
                    section: here.section,
                    subsection: here.subsection,
                    key,
                    value,
                    origin: None,
                };
                out.push((line, entry));
            }
        }
    }

    fn parse_section_header(&mut self) -> Result<SectionCtx, ConfigError> {
        self.pos += 1; // '['
        let mut name = Vec::new();
        loop {
            match self
                .next()
                .ok_or(ConfigError::Syntax("unterminated section header"))?
            {
                b']' if name.is_empty() => {
                    return Err(ConfigError::Syntax("empty section name"));
                }
                b']' => {
                    // possibly legacy `[section.subsection]`: lowercased
                    // wholesale, split at the first dot
                    let lower = name.to_ascii_lowercase();
                    let lower = String::from_utf8(lower)
                        .map_err(|_| ConfigError::Syntax("non-ascii section name"))?;
                    return Ok(match lower.split_once('.') {
                        Some((sec, sub)) => SectionCtx {
                            section: sec.to_owned(),
                            subsection: Some(sub.into()),
                        },
                        None => SectionCtx {
                            section: lower,
                            subsection: None,
                        },
                    });
                }
                b' ' | b'\t' => {
                    // `[section "subsection"]`
                    self.skip_inline_ws();
                    if self.next() != Some(b'"') {
                        return Err(ConfigError::Syntax("expected quoted subsection"));
                    }
                    let mut sub = Vec::new();
                    loop {
                        match self
                            .next()
                            .ok_or(ConfigError::Syntax("unterminated subsection"))?
                        {
                            b'"' => break,
                            b'\\' => sub.push(
                                self.next()
                                    .ok_or(ConfigError::Syntax("unterminated subsection"))?,
                            ),
                            b'\n' => return Err(ConfigError::Syntax("newline in subsection")),
                            c => sub.push(c),
                        }
                    }
                    if self.next() != Some(b']') {
                        return Err(ConfigError::Syntax("expected ] after subsection"));
                    }
                    let section = String::from_utf8(name.to_ascii_lowercase())
                        .map_err(|_| ConfigError::Syntax("non-ascii section name"))?;
                    return Ok(SectionCtx {
                        section,
                        subsection: Some(sub.into()),
                    });
                }
                c if c.is_ascii_alphanumeric() || c == b'-' || c == b'.' => name.push(c),
                _ => return Err(ConfigError::Syntax("invalid section name character")),
            }
        }
    }

    fn parse_variable(&mut self) -> Result<(String, Option<BString>), ConfigError> {
        let mut key = Vec::new();
        let first = self.next().expect("caller peeked");
        if !first.is_ascii_alphabetic() {
            return Err(ConfigError::Syntax("variable must start with a letter"));
        }
        key.push(first.to_ascii_lowercase());
        loop {
            match self.peek() {
                Some(c) if c.is_ascii_alphanumeric() || c == b'-' => {
                    key.push(c.to_ascii_lowercase());
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let key = String::from_utf8(key).expect("ascii by construction");
        self.skip_inline_ws();

        match self.peek() {
            None | Some(b'\n') => {
                // valueless boolean: `[x] flag`
                Ok((key, None))
            }
            Some(b'=') => {
                self.pos += 1;
                let value = self.parse_value()?;
                Ok((key, Some(value)))
            }
            _ => Err(ConfigError::Syntax(
                "unexpected character after variable name",
            )),
        }
    }

    /// Value, with git's exact whitespace semantics: each run of unquoted
    /// whitespace becomes that many `' '` bytes (tabs included), leading and
    /// trailing unquoted whitespace is dropped. `"` quoting, `\` escapes
    /// (`\n \t \b \" \\`), backslash-newline continuation, `#`/`;` comments
    /// outside quotes.
    fn parse_value(&mut self) -> Result<BString, ConfigError> {
        let mut out: Vec<u8> = Vec::new();
        let mut quoted = false;
        // unquoted whitespace is buffered verbatim and flushed before the
        // next real byte — so it vanishes at EOL and at start, but interior
        // tabs survive as tabs (verified against git's own --list output)
        let mut pending: Vec<u8> = Vec::new();
        let flush = |out: &mut Vec<u8>, pending: &mut Vec<u8>| {
            out.append(pending);
        };
        while let Some(c) = self.peek() {
            match c {
                b'\n' => break,
                b'#' | b';' if !quoted => {
                    self.skip_to_newline();
                    break;
                }
                _ if is_space(c) && !quoted => {
                    if !out.is_empty() {
                        pending.push(c);
                    }
                    self.pos += 1;
                }
                b'"' => {
                    flush(&mut out, &mut pending);
                    quoted = !quoted;
                    self.pos += 1;
                }
                b'\\' => {
                    flush(&mut out, &mut pending);
                    self.pos += 1;
                    // the end of the file ends a line as a newline does
                    match self.next().unwrap_or(b'\n') {
                        b'\n' => {} // line continuation
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        _ => return Err(ConfigError::Syntax("invalid escape in value")),
                    }
                }
                _ => {
                    flush(&mut out, &mut pending);
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
        if quoted {
            return Err(ConfigError::Syntax("unterminated quote in value"));
        }
        Ok(out.into())
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ') | Some(b'\t')) {
            self.pos += 1;
        }
    }

    fn skip_to_newline(&mut self) {
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.pos += 1;
        }
    }

    fn skip_ws_comments_and_newlines(&mut self) {
        loop {
            match self.peek() {
                Some(b) if is_space(b) => self.pos += 1,
                Some(b'#') | Some(b';') => self.skip_to_newline(),
                _ => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(s: &str) -> Vec<Entry> {
        parse_file(s.as_bytes()).unwrap()
    }

    #[test]
    fn sections_keys_and_values() {
        let got = entries(
            "[core]\n\
             \tbare = false\n\
             [branch \"ma in\"]\n\
             remote = origin\n\
             [Legacy.Sub]\n\
             KEY = v\n",
        );
        assert_eq!(got.len(), 3);
        assert_eq!(
            (got[0].section.as_str(), got[0].key.as_str()),
            ("core", "bare")
        );
        let sub = |i: usize| got[i].subsection.as_ref().map(|s| s.as_slice());
        assert_eq!(sub(1), Some(b"ma in".as_slice()));
        // legacy dotted form: lowercased wholesale
        assert_eq!(got[2].section, "legacy");
        assert_eq!(sub(2), Some(b"sub".as_slice()));
        assert_eq!(got[2].key, "key");
    }

    #[test]
    fn value_quoting_escapes_comments_continuation() {
        let got = entries(
            "[x]\n\
             a = plain value   # comment\n\
             b = \"kept   \" ; comment\n\
             c = with \"quo;ted\" part\n\
             d = esc\\n\\t\\\\\\\"end\n\
             e = one \\\n two\n\
             f = tab\tseparated\n\
             flag\n\
             empty =\n",
        );
        let v = |i: usize| got[i].value.as_ref().map(|v| v.to_string());
        assert_eq!(v(0).unwrap(), "plain value");
        assert_eq!(v(1).unwrap(), "kept   ");
        assert_eq!(v(2).unwrap(), "with quo;ted part");
        assert_eq!(v(3).unwrap(), "esc\n\t\\\"end");
        assert_eq!(v(4).unwrap(), "one  two");
        // interior unquoted tabs survive verbatim (matches git --list)
        assert_eq!(v(5).unwrap(), "tab\tseparated");
        assert_eq!(got[6].value, None);
        assert_eq!(v(7).unwrap(), "");
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse_file(b"[]\na = 1\n").is_err());
        assert!(parse_file(b"[x \"y\" ]\na = 1\n").is_err());
        assert!(parse_file(b"[x]\nflag # said\n").is_err());
        assert!(parse_file(b"[x]\na_b = 1\n").is_err());
        assert!(parse_file(b"[unterminated\n").is_err());
        assert!(parse_file(b"[x]\na = \"open\n").is_err());
        assert!(parse_file(b"[x]\na = bad\\zescape\n").is_err());
        assert!(parse_file(b"[x]\n1leading = digit\n").is_err());
    }

    #[test]
    fn what_git_reads_is_read() {
        // a byte order mark, lines that end in CR LF, also behind a backslash
        let got = entries("\u{feff}[a]\r\nflag\r\nx = 1\r\ny = a\\\r\nb\r\n");
        let v = |i: usize| got[i].value.as_ref().map(|v| v.to_string());
        assert_eq!(got.len(), 3);
        assert_eq!(
            (v(0), v(1), v(2)),
            (None, Some("1".into()), Some("ab".into()))
        );
        // a variable before any section, which no lookup finds
        let got = entries("x = 1\n[a]\ny = 2\n");
        assert_eq!((got[0].section.as_str(), got[0].key.as_str()), ("", "x"));
        assert_eq!(got[1].section, "a");
        // a backslash that ends the file, and a tab before a subsection
        assert_eq!(
            entries("[a]\nx = 1\\")[0]
                .value
                .as_ref()
                .unwrap()
                .to_string(),
            "1"
        );
        let got = entries("[a]\nx = \u{b}1\n[b\t\"c\"] y = 2\n");
        assert_eq!(got[0].value.as_ref().unwrap().to_string(), "\u{b}1");
        assert_eq!(got[1].subsection.as_ref().unwrap().to_string(), "c");
    }

    #[test]
    fn an_error_names_the_line_git_names() {
        for (text, line) in [
            ("[core\n", 1),
            ("[a]\nx = 1\n[b]\ny = \"open\n", 4),
            ("[a]\n\n1x = 2\n", 3),
            ("[a]\nflag ; said\n", 2),
            ("[a]\r\nx = 1\r\ny = bad\\z\r\n", 3),
        ] {
            assert_eq!(
                parse_located(text.as_bytes()).unwrap_err().0,
                line,
                "{text:?}"
            );
        }
    }
}
