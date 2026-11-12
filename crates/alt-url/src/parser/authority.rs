use super::{Parser, default_port};
use crate::percent::{self, EncodeSet};
use crate::{ParseError, host};

/// Splits `host[:port]` at the first `:` outside brackets.
fn split_port(input: &str) -> (&str, Option<&str>) {
    let mut bracketed = false;
    for (i, byte) in input.bytes().enumerate() {
        match byte {
            b'[' => bracketed = true,
            b']' => bracketed = false,
            b':' if !bracketed => return (&input[..i], Some(&input[i + 1..])),
            _ => {}
        }
    }
    (input, None)
}

impl Parser {
    /// Parses the authority at the start of `input` and the path after it.
    pub(super) fn authority<'i>(&mut self, input: &'i str) -> Result<&'i str, ParseError> {
        let end = input
            .bytes()
            .position(|b| matches!(b, b'?' | b'#') || self.is_slash(b))
            .unwrap_or(input.len());
        let (authority, rest) = input.split_at(end);
        self.out.push_str("//");
        self.authority = true;
        let host_port = match authority.rfind('@') {
            Some(at) => {
                self.userinfo(&authority[..at]);
                if at + 1 == authority.len() {
                    return Err(ParseError::EmptyHost);
                }
                &authority[at + 1..]
            }
            None => {
                self.username_end = self.out.len();
                authority
            }
        };
        self.host_start = self.out.len();
        let (host, port) = split_port(host_port);
        if host.is_empty() && (self.special || port.is_some()) {
            return Err(ParseError::EmptyHost);
        }
        self.host = host::parse(&mut self.out, host, self.special)?;
        self.host_end = self.out.len();
        if let Some(port) = port {
            self.port(port)?;
        }
        Ok(self.path_start(rest))
    }

    fn userinfo(&mut self, input: &str) {
        let (username, password) = input.split_once(':').unwrap_or((input, ""));
        percent::encode(&mut self.out, username, EncodeSet::USERINFO);
        self.username_end = self.out.len();
        if !password.is_empty() {
            self.out.push(':');
            percent::encode(&mut self.out, password, EncodeSet::USERINFO);
        }
        if !input.is_empty() && input != ":" {
            self.out.push('@');
        }
    }

    fn port(&mut self, input: &str) -> Result<(), ParseError> {
        if input.is_empty() {
            return Ok(());
        }
        let mut value = 0u32;
        for byte in input.bytes() {
            if !byte.is_ascii_digit() {
                return Err(ParseError::InvalidPort);
            }
            value = value * 10 + u32::from(byte - b'0');
            if value > 65535 {
                return Err(ParseError::InvalidPort);
            }
        }
        let value = value as u16;
        if default_port(&self.out[..self.scheme_end]) != Some(value) {
            self.port = Some(value);
            self.out.push(':');
            let mut digits = [0u8; 5];
            let mut at = digits.len();
            let mut left = value;
            loop {
                at -= 1;
                digits[at] = b'0' + (left % 10) as u8;
                left /= 10;
                if left == 0 {
                    break;
                }
            }
            for &digit in &digits[at..] {
                self.out.push(digit as char);
            }
        }
        Ok(())
    }
}
