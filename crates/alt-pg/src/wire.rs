//! Frontend message framing and a bounds-checked reader for backend bodies.

use crate::{Error, Result, ServerError};

/// the largest message body the server itself will produce
pub(crate) const MAX_MESSAGE: usize = 0x3fff_ffff;

/// starts a message and returns where its length goes
pub(crate) fn begin(out: &mut Vec<u8>, tag: u8) -> usize {
    out.push(tag);
    let at = out.len();
    out.extend_from_slice(&[0; 4]);
    at
}

/// fills in the length of the message started at `at`
pub(crate) fn finish(out: &mut [u8], at: usize) -> Result<()> {
    let length = out.len() - at;
    if length > MAX_MESSAGE {
        return Err(Error::Request("message too long"));
    }
    out[at..at + 4].copy_from_slice(&(length as u32).to_be_bytes());
    Ok(())
}

pub(crate) fn cstring(out: &mut Vec<u8>, text: &[u8]) -> Result<()> {
    if text.contains(&0) {
        return Err(Error::Request("text holds a NUL byte"));
    }
    out.extend_from_slice(text);
    out.push(0);
    Ok(())
}

/// a prepared statement's name on the wire; zero is the unnamed statement
pub(crate) fn statement_name(out: &mut Vec<u8>, id: u32) {
    if id != 0 {
        out.push(b's');
        let mut digits = [0u8; 10];
        let mut at = digits.len();
        let mut rest = id;
        while rest > 0 {
            at -= 1;
            digits[at] = b'0' + (rest % 10) as u8;
            rest /= 10;
        }
        out.extend_from_slice(&digits[at..]);
    }
    out.push(0);
}

pub(crate) fn parse(out: &mut Vec<u8>, id: u32, sql: &str) -> Result<()> {
    let at = begin(out, b'P');
    statement_name(out, id);
    cstring(out, sql.as_bytes())?;
    out.extend_from_slice(&0i16.to_be_bytes());
    finish(out, at)
}

pub(crate) fn describe_statement(out: &mut Vec<u8>, id: u32) -> Result<()> {
    let at = begin(out, b'D');
    out.push(b'S');
    statement_name(out, id);
    finish(out, at)
}

pub(crate) fn close_statement(out: &mut Vec<u8>, id: u32) -> Result<()> {
    let at = begin(out, b'C');
    out.push(b'S');
    statement_name(out, id);
    finish(out, at)
}

/// executes the unnamed portal to completion
pub(crate) fn execute(out: &mut Vec<u8>) {
    out.extend_from_slice(&[b'E', 0, 0, 0, 9, 0, 0, 0, 0, 0]);
}

pub(crate) fn sync(out: &mut Vec<u8>) {
    out.extend_from_slice(&[b'S', 0, 0, 0, 4]);
}

pub(crate) fn query(out: &mut Vec<u8>, sql: &[&str]) -> Result<()> {
    let at = begin(out, b'Q');
    for part in sql {
        if part.as_bytes().contains(&0) {
            return Err(Error::Request("text holds a NUL byte"));
        }
        out.extend_from_slice(part.as_bytes());
    }
    out.push(0);
    finish(out, at)
}

/// opens a transaction block inside the current extended-protocol batch, so
/// a failure to open it skips every later message up to the sync
pub(crate) fn begin_transaction(out: &mut Vec<u8>) {
    out.extend_from_slice(b"P\0\0\0\x0d\0BEGIN\0\0\0");
    out.extend_from_slice(b"B\0\0\0\x0c\0\0\0\0\0\0\0\0");
    execute(out);
}

/// binds and runs a prepared statement that takes no parameters; any rows come in binary
pub(crate) fn run_plain(out: &mut Vec<u8>, id: u32) {
    let at = begin(out, b'B');
    out.push(0);
    statement_name(out, id);
    out.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 1]);
    finish(out, at).expect("a short message");
    execute(out);
}

pub(crate) fn terminate(out: &mut Vec<u8>) {
    out.extend_from_slice(&[b'X', 0, 0, 0, 4]);
}

/// reads fields out of one message body without ever indexing past its end
pub(crate) struct Body<'a> {
    bytes: &'a [u8],
}

const TRUNCATED: Error = Error::Protocol("message shorter than its fields");

impl<'a> Body<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
    pub fn remaining(&self) -> usize {
        self.bytes.len()
    }
    pub fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let (head, rest) = self.bytes.split_at_checked(count).ok_or(TRUNCATED)?;
        self.bytes = rest;
        Ok(head)
    }
    pub fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.bytes)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }
    /// a NUL-terminated string, without the terminator
    pub fn cstr(&mut self) -> Result<&'a [u8]> {
        let end = self
            .bytes
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(Error::Protocol("unterminated string"))?;
        let text = &self.bytes[..end];
        self.bytes = &self.bytes[end + 1..];
        Ok(text)
    }
    pub fn text(&mut self) -> Result<&'a str> {
        std::str::from_utf8(self.cstr()?).map_err(|_| Error::Protocol("string is not UTF-8"))
    }
    pub fn end(&self) -> Result<()> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(Error::Protocol("message longer than its fields"))
        }
    }
}

pub(crate) fn server_error(body: &[u8]) -> Result<ServerError> {
    let mut body = Body::new(body);
    let mut severity = None;
    let mut localized = None;
    let mut code = None;
    let mut message = None;
    let mut error = ServerError {
        severity: String::new(),
        code: String::new(),
        message: String::new(),
        detail: None,
        hint: None,
        table: None,
        column: None,
        constraint: None,
    };
    loop {
        let field = body.u8()?;
        if field == 0 {
            break;
        }
        let value = body.text()?;
        match field {
            b'V' => severity = Some(value),
            b'S' => localized = Some(value),
            b'C' => code = Some(value),
            b'M' => message = Some(value),
            b'D' => error.detail = Some(value.into()),
            b'H' => error.hint = Some(value.into()),
            b't' => error.table = Some(value.into()),
            b'c' => error.column = Some(value.into()),
            b'n' => error.constraint = Some(value.into()),
            _ => {}
        }
    }
    body.end()?;
    let code = code.filter(|code| code.len() == 5 && code.is_ascii());
    error.code = code
        .ok_or(Error::Protocol("error without SQLSTATE"))?
        .into();
    error.message = message
        .ok_or(Error::Protocol("error without message"))?
        .into();
    error.severity = severity
        .or(localized)
        .ok_or(Error::Protocol("error without severity"))?
        .into();
    Ok(error)
}

/// the row count at the end of a command tag, zero when it has none
pub(crate) fn affected(body: &[u8]) -> Result<u64> {
    let tag = Body::new(body).cstr()?;
    let digits = tag.rsplit(|&byte| byte == b' ').next().unwrap_or(&[]);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Ok(0);
    }
    digits
        .iter()
        .try_fold(0u64, |count, &digit| {
            count.checked_mul(10)?.checked_add(u64::from(digit - b'0'))
        })
        .ok_or(Error::Protocol("row count out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_carry_their_own_length() {
        let mut out = Vec::new();
        parse(&mut out, 12, "SELECT 1").unwrap();
        assert_eq!(out, b"P\0\0\0\x13s12\0SELECT 1\0\0\0");
        out.clear();
        parse(&mut out, 0, "BEGIN").unwrap();
        let mut begin = Vec::new();
        begin_transaction(&mut begin);
        assert!(begin.starts_with(&out));
        assert_eq!(begin.len(), out.len() + 13 + 10);
        out.clear();
        describe_statement(&mut out, 4_000_000_000).unwrap();
        assert_eq!(out, b"D\0\0\0\x11Ss4000000000\0");
        out.clear();
        query(&mut out, &["BEGIN;", "SELECT 1"]).unwrap();
        assert_eq!(out, b"Q\0\0\0\x13BEGIN;SELECT 1\0");
        assert!(parse(&mut out, 1, "SELECT '\0'").is_err());
    }

    #[test]
    fn body_reads_stop_at_the_end() {
        let mut body = Body::new(&[0, 0, 0, 5, b'a', 0, 9]);
        assert_eq!(body.i32().unwrap(), 5);
        assert_eq!(body.cstr().unwrap(), b"a");
        assert!(body.i16().is_err());
        assert!(body.end().is_err());
        assert_eq!(body.u8().unwrap(), 9);
        body.end().unwrap();
        assert!(body.u8().is_err());
        assert!(Body::new(b"abc").cstr().is_err());
        assert!(Body::new(&[0xff, 0]).text().is_err());
    }

    #[test]
    fn error_fields_are_required_and_bounded() {
        let error =
            server_error(b"SERROR\0VERROR\0C23505\0Mduplicate\0nrefs_pkey\0Ddetail\0\0").unwrap();
        assert_eq!(error.code, "23505");
        assert_eq!(error.message, "duplicate");
        assert_eq!(error.constraint.as_deref(), Some("refs_pkey"));
        assert_eq!(error.detail.as_deref(), Some("detail"));
        for malformed in [
            &b""[..],
            b"SERROR\0C23505\0Mno terminator\0",
            b"SERROR\0Mmissing code\0\0",
            b"SERROR\0C2350\0Mshort code\0\0",
            b"C23505\0Mno severity\0\0",
            b"SERROR\0C23505\0Munterminated",
            b"SERROR\0C23505\0Mtrailing\0\0x",
        ] {
            assert!(server_error(malformed).is_err(), "{malformed:?}");
        }
    }

    #[test]
    fn command_tags_end_in_the_row_count() {
        assert_eq!(affected(b"INSERT 0 3\0").unwrap(), 3);
        assert_eq!(affected(b"UPDATE 12\0").unwrap(), 12);
        assert_eq!(affected(b"SELECT 500\0").unwrap(), 500);
        assert_eq!(affected(b"BEGIN\0").unwrap(), 0);
        assert_eq!(affected(b"CREATE TABLE\0").unwrap(), 0);
        assert!(affected(b"UPDATE 12").is_err());
        assert!(affected(b"UPDATE 99999999999999999999999\0").is_err());
    }
}
