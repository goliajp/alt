use crate::percent::{self, EncodeSet};
use crate::{ParseError, idna, ip};
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Host<S = String> {
    Domain(S),
    Ipv4(Ipv4Addr),
    Ipv6(Ipv6Addr),
}

impl Host<&str> {
    pub fn to_owned(&self) -> Host<String> {
        match *self {
            Host::Domain(domain) => Host::Domain(domain.to_owned()),
            Host::Ipv4(address) => Host::Ipv4(address),
            Host::Ipv6(address) => Host::Ipv6(address),
        }
    }
}

impl<S: AsRef<str>> fmt::Display for Host<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = String::new();
        match self {
            Host::Domain(domain) => return f.write_str(domain.as_ref()),
            Host::Ipv4(address) => ip::write_ipv4(&mut text, *address),
            Host::Ipv6(address) => {
                text.push('[');
                ip::write_ipv6(&mut text, *address);
                text.push(']');
            }
        }
        f.write_str(&text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum HostKind {
    None,
    Domain,
    Ipv4(Ipv4Addr),
    Ipv6(Ipv6Addr),
}

const fn mask(bytes: &[u8]) -> u128 {
    let mut bits = 0;
    let mut i = 0;
    while i < bytes.len() {
        bits |= 1 << bytes[i];
        i += 1;
    }
    bits
}

const FORBIDDEN_HOST: u128 = mask(b"\0\t\n\r #/:<>?@[\\]^|");
const FORBIDDEN_DOMAIN: u128 = FORBIDDEN_HOST | ((1 << 0x20) - 1) | mask(b"%\x7f");

#[inline]
fn forbidden(set: u128, byte: u8) -> bool {
    byte < 0x80 && (set >> byte) & 1 != 0
}

/// Parses `input` as a host and appends its serialization to `out`.
pub(crate) fn parse(out: &mut String, input: &str, special: bool) -> Result<HostKind, ParseError> {
    if let Some(rest) = input.strip_prefix('[') {
        let inner = rest
            .strip_suffix(']')
            .ok_or(ParseError::InvalidIpv6Address)?;
        let address = ip::parse_ipv6(inner.as_bytes())?;
        out.push('[');
        ip::write_ipv6(out, address);
        out.push(']');
        return Ok(HostKind::Ipv6(address));
    }
    if special {
        return domain(out, input);
    }
    if input.bytes().any(|byte| forbidden(FORBIDDEN_HOST, byte)) {
        return Err(ParseError::InvalidDomainCharacter);
    }
    percent::encode(out, input, EncodeSet::C0_CONTROL);
    Ok(if input.is_empty() {
        HostKind::None
    } else {
        HostKind::Domain
    })
}

fn domain(out: &mut String, input: &str) -> Result<HostKind, ParseError> {
    let start = out.len();
    let decoded = percent::decode(input.as_bytes());
    if decoded.is_ascii() {
        for &byte in decoded.iter() {
            if forbidden(FORBIDDEN_DOMAIN, byte) {
                return Err(ParseError::InvalidDomainCharacter);
            }
            out.push(byte.to_ascii_lowercase() as char);
        }
    } else {
        let text = std::str::from_utf8(&decoded).map_err(|_| ParseError::IdnaError)?;
        idna::to_ascii(text, out)?;
        if out[start..]
            .bytes()
            .any(|byte| forbidden(FORBIDDEN_DOMAIN, byte))
        {
            return Err(ParseError::InvalidDomainCharacter);
        }
    }
    if out.len() == start {
        return Err(ParseError::EmptyHost);
    }
    if ip::ends_in_number(&out[start..]) {
        let address = ip::parse_ipv4(&out[start..])?;
        out.truncate(start);
        ip::write_ipv4(out, address);
        return Ok(HostKind::Ipv4(address));
    }
    Ok(HostKind::Domain)
}
