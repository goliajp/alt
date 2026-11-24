//! The workflow.toml subset, read into flat rows of a key path and a value.

use std::collections::BTreeMap;

use super::{Position, Severity, TomlRow, TomlSeg, TomlVal, WfDiag};

/// Walk the source character by character, producing one [`TomlRow`]
/// per `key = value` line and reshuffling `[section]` / `[[array]]`
/// headers into the current path prefix. Errors during the lexical
/// walk (unterminated string, unknown value, dangling `=`) become
/// [`Severity::Error`] diagnostics; recovery skips to the next
/// newline so the rest of the file is still validated.
pub(super) fn parse_toml_subset(src: &str) -> (Vec<TomlRow>, Vec<WfDiag>) {
    let mut p = P {
        src: src.as_bytes(),
        at: 0,
    };
    let mut rows = Vec::new();
    let mut diags = Vec::new();
    let mut prefix: Vec<TomlSeg> = Vec::new();
    let mut arr_counts: BTreeMap<String, usize> = BTreeMap::new();

    while p.at < p.src.len() {
        p.skip_blanks_and_comments();
        if p.at >= p.src.len() {
            break;
        }
        match p.peek() {
            Some(b'[') => {
                let header_at = p.at;
                let is_array = p.src.get(p.at + 1) == Some(&b'[');
                p.at += if is_array { 2 } else { 1 };
                p.skip_inline_ws();
                let key = match p.read_bare_or_quoted_key() {
                    Some(k) => k,
                    None => {
                        diags.push(WfDiag {
                            at: Position::at(header_at),
                            severity: Severity::Error,
                            message: "expected key inside section header".to_owned(),
                        });
                        p.skip_to_eol();
                        continue;
                    }
                };
                p.skip_inline_ws();
                if p.peek() != Some(b']') {
                    diags.push(WfDiag {
                        at: Position::at(p.at),
                        severity: Severity::Error,
                        message: "expected `]` to close section header".to_owned(),
                    });
                    p.skip_to_eol();
                    continue;
                }
                p.at += 1;
                if is_array {
                    if p.peek() != Some(b']') {
                        diags.push(WfDiag {
                            at: Position::at(p.at),
                            severity: Severity::Error,
                            message: "expected `]]` to close array-of-tables header".to_owned(),
                        });
                        p.skip_to_eol();
                        continue;
                    }
                    p.at += 1;
                }
                p.skip_to_eol();
                prefix.clear();
                if is_array {
                    let idx = arr_counts.entry(key.clone()).or_insert(0);
                    let here = *idx;
                    *idx += 1;
                    prefix.push(TomlSeg::ArrIdx(key, here));
                } else {
                    prefix.push(TomlSeg::Key(key));
                }
            }
            Some(_) => {
                let key_at = p.at;
                let key = match p.read_bare_or_quoted_key() {
                    Some(k) => k,
                    None => {
                        diags.push(WfDiag {
                            at: Position::at(key_at),
                            severity: Severity::Error,
                            message: "expected key".to_owned(),
                        });
                        p.skip_to_eol();
                        continue;
                    }
                };
                // dotted key: `a.b = ...` collapses into nested
                // segments under the current prefix.
                let mut tail: Vec<TomlSeg> = vec![TomlSeg::Key(key)];
                p.skip_inline_ws();
                while p.peek() == Some(b'.') {
                    p.at += 1;
                    p.skip_inline_ws();
                    match p.read_bare_or_quoted_key() {
                        Some(k) => tail.push(TomlSeg::Key(k)),
                        None => {
                            diags.push(WfDiag {
                                at: Position::at(p.at),
                                severity: Severity::Error,
                                message: "expected key after `.`".to_owned(),
                            });
                            p.skip_to_eol();
                            break;
                        }
                    }
                    p.skip_inline_ws();
                }
                p.skip_inline_ws();
                if p.peek() != Some(b'=') {
                    diags.push(WfDiag {
                        at: Position::at(p.at),
                        severity: Severity::Error,
                        message: "expected `=` after key".to_owned(),
                    });
                    p.skip_to_eol();
                    continue;
                }
                p.at += 1;
                p.skip_inline_ws();
                let value_at = p.at;
                let value = match p.read_value() {
                    Ok(v) => v,
                    Err(msg) => {
                        diags.push(WfDiag {
                            at: Position::at(value_at),
                            severity: Severity::Error,
                            message: msg,
                        });
                        p.skip_to_eol();
                        continue;
                    }
                };
                p.skip_to_eol();
                let mut full = prefix.clone();
                full.extend(tail);
                rows.push(TomlRow {
                    path: full,
                    value,
                    at: Position::at(key_at),
                });
            }
            None => break,
        }
    }

    (rows, diags)
}

struct P<'a> {
    src: &'a [u8],
    at: usize,
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.at).copied()
    }

    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.at += 1;
        }
    }

    fn skip_blanks_and_comments(&mut self) {
        loop {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.at += 1;
            }
            if self.peek() == Some(b'#') {
                while let Some(c) = self.peek() {
                    self.at += 1;
                    if c == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn skip_to_eol(&mut self) {
        while let Some(c) = self.peek() {
            self.at += 1;
            if c == b'\n' {
                return;
            }
        }
    }

    fn read_bare_or_quoted_key(&mut self) -> Option<String> {
        match self.peek()? {
            b'"' => self.read_quoted_string().ok(),
            c if is_bare_key_byte(c) => {
                let start = self.at;
                while let Some(c) = self.peek() {
                    if is_bare_key_byte(c) {
                        self.at += 1;
                    } else {
                        break;
                    }
                }
                Some(
                    std::str::from_utf8(&self.src[start..self.at])
                        .ok()?
                        .to_owned(),
                )
            }
            _ => None,
        }
    }

    fn read_value(&mut self) -> Result<TomlVal, String> {
        match self.peek() {
            Some(b'"') => self.read_quoted_string().map(TomlVal::Str),
            Some(b'[') => self.read_string_array().map(TomlVal::Arr),
            Some(_) => Err("unsupported value (only strings and string arrays)".to_owned()),
            None => Err("unexpected end of file in value".to_owned()),
        }
    }

    fn read_quoted_string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err("expected `\"`".to_owned());
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".to_owned()),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    match self.peek() {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'n') => out.push('\n'),
                        Some(b't') => out.push('\t'),
                        Some(b'r') => out.push('\r'),
                        Some(c) => {
                            return Err(format!("unrecognised escape `\\{}`", char::from(c)));
                        }
                        None => return Err("unterminated escape in string".to_owned()),
                    }
                    self.at += 1;
                }
                Some(b'\n') => return Err("multi-line strings not supported".to_owned()),
                Some(c) => {
                    out.push(char::from(c));
                    self.at += 1;
                }
            }
        }
    }

    fn read_string_array(&mut self) -> Result<Vec<String>, String> {
        if self.peek() != Some(b'[') {
            return Err("expected `[`".to_owned());
        }
        self.at += 1;
        let mut out = Vec::new();
        loop {
            self.skip_array_ws();
            match self.peek() {
                None => return Err("unterminated array".to_owned()),
                Some(b']') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'"') => {
                    out.push(self.read_quoted_string()?);
                    self.skip_array_ws();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {}
                        _ => {
                            return Err("expected `,` or `]` after array element".to_owned());
                        }
                    }
                }
                _ => return Err("only string arrays are supported".to_owned()),
            }
        }
    }

    fn skip_array_ws(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.at += 1,
                Some(b'#') => {
                    while let Some(c) = self.peek() {
                        self.at += 1;
                        if c == b'\n' {
                            break;
                        }
                    }
                }
                _ => return,
            }
        }
    }
}

fn is_bare_key_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}
