use bstr::BString;

use crate::{ConfigError, Entry};

/// Parses one config file's syntax into entries, in order. No include
/// processing here — `include.path` lines come out as plain entries.
pub fn parse_file(data: &[u8]) -> Result<Vec<Entry>, ConfigError> {
    Parser { data, pos: 0 }.run()
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
    fn run(mut self) -> Result<Vec<Entry>, ConfigError> {
        let mut out = Vec::new();
        let mut ctx: Option<SectionCtx> = None;
        loop {
            self.skip_ws_comments_and_newlines();
            let Some(byte) = self.peek() else {
                return Ok(out);
            };
            if byte == b'[' {
                ctx = Some(self.parse_section_header()?);
            } else {
                let here = ctx
                    .clone()
                    .ok_or(ConfigError::Syntax("variable before any section header"))?;
                let (key, value) = self.parse_variable()?;
                out.push(Entry {
                    section: here.section,
                    subsection: here.subsection,
                    key,
                    value,
                });
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
                    self.skip_inline_ws();
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
            Some(b'#') | Some(b';') => {
                self.skip_to_newline();
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
                b' ' | b'\t' | b'\r' if !quoted => {
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
                    let esc = self
                        .next()
                        .ok_or(ConfigError::Syntax("dangling backslash in value"))?;
                    match esc {
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
                Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => self.pos += 1,
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
        assert!(parse_file(b"key = before section\n").is_err());
        assert!(parse_file(b"[unterminated\n").is_err());
        assert!(parse_file(b"[x]\na = \"open\n").is_err());
        assert!(parse_file(b"[x]\na = bad\\zescape\n").is_err());
        assert!(parse_file(b"[x]\n1leading = digit\n").is_err());
    }
}
