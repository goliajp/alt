use crate::ParseError;
use crate::percent::hex_value;
use std::net::{Ipv4Addr, Ipv6Addr};

/// Whether a domain's last label is a number, which makes the host an IPv4 address.
pub(crate) fn ends_in_number(domain: &str) -> bool {
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    let last = domain.rsplit('.').next().unwrap_or(domain).as_bytes();
    if last.is_empty() {
        return false;
    }
    if last.iter().all(u8::is_ascii_digit) {
        return true;
    }
    matches!(last, [b'0', b'x' | b'X', hex @ ..] if hex.iter().all(u8::is_ascii_hexdigit))
}

fn ipv4_number(part: &[u8]) -> Option<u64> {
    let (radix, digits) = match part {
        [] => return None,
        [b'0', b'x' | b'X', rest @ ..] => (16, rest),
        [b'0', rest @ ..] if !rest.is_empty() => (8, rest),
        _ => (10, part),
    };
    let mut value = 0u64;
    for &digit in digits {
        let digit = hex_value(digit).filter(|&d| d < radix)?;
        value = value
            .saturating_mul(u64::from(radix))
            .saturating_add(u64::from(digit));
    }
    Some(value)
}

pub(crate) fn parse_ipv4(input: &str) -> Result<Ipv4Addr, ParseError> {
    let input = input.strip_suffix('.').unwrap_or(input);
    let mut parts = [0u64; 4];
    let mut count = 0;
    for part in input.split('.') {
        if count == 4 {
            return Err(ParseError::InvalidIpv4Address);
        }
        parts[count] = ipv4_number(part.as_bytes()).ok_or(ParseError::InvalidIpv4Address)?;
        count += 1;
    }
    let last = parts[count - 1];
    if parts[..count - 1].iter().any(|&part| part > 255) || last >= 1 << (8 * (5 - count)) {
        return Err(ParseError::InvalidIpv4Address);
    }
    let mut address = last;
    for (i, part) in parts[..count - 1].iter().enumerate() {
        address |= part << (8 * (3 - i));
    }
    Ok(Ipv4Addr::from(address as u32))
}

fn push_decimal(out: &mut String, value: u8) {
    if value >= 100 {
        out.push((b'0' + value / 100) as char);
    }
    if value >= 10 {
        out.push((b'0' + value / 10 % 10) as char);
    }
    out.push((b'0' + value % 10) as char);
}

pub(crate) fn write_ipv4(out: &mut String, address: Ipv4Addr) {
    for (i, octet) in address.octets().into_iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        push_decimal(out, octet);
    }
}

/// Reads the dotted IPv4 tail of an IPv6 address into two pieces.
fn ipv4_tail(input: &[u8], mut at: usize, pieces: &mut [u16]) -> Option<()> {
    let mut seen = 0;
    while at < input.len() {
        if seen > 0 {
            if input[at] != b'.' || seen == 4 {
                return None;
            }
            at += 1;
        }
        let mut value: Option<u16> = None;
        while let Some(digit) = input.get(at).filter(|b| b.is_ascii_digit()) {
            let digit = u16::from(digit - b'0');
            value = Some(match value {
                None => digit,
                Some(0) => return None,
                Some(value) => value * 10 + digit,
            });
            if value > Some(255) {
                return None;
            }
            at += 1;
        }
        let piece = &mut pieces[seen / 2];
        *piece = *piece * 0x100 + value?;
        seen += 1;
    }
    (seen == 4).then_some(())
}

pub(crate) fn parse_ipv6(input: &[u8]) -> Result<Ipv6Addr, ParseError> {
    ipv6_pieces(input)
        .map(Ipv6Addr::from)
        .ok_or(ParseError::InvalidIpv6Address)
}

fn ipv6_pieces(input: &[u8]) -> Option<[u16; 8]> {
    let mut pieces = [0u16; 8];
    let mut piece = 0;
    let mut compress = None;
    let mut at = 0;
    if input.first() == Some(&b':') {
        if input.get(1) != Some(&b':') {
            return None;
        }
        at = 2;
        piece = 1;
        compress = Some(1);
    }
    while at < input.len() {
        if piece == 8 {
            return None;
        }
        if input[at] == b':' {
            if compress.is_some() {
                return None;
            }
            at += 1;
            piece += 1;
            compress = Some(piece);
            continue;
        }
        let start = at;
        let mut value = 0u16;
        while at - start < 4
            && let Some(digit) = input.get(at).copied().and_then(hex_value)
        {
            value = value * 16 + u16::from(digit);
            at += 1;
        }
        match input.get(at) {
            Some(b'.') => {
                if at == start || piece > 6 {
                    return None;
                }
                ipv4_tail(input, start, &mut pieces[piece..piece + 2])?;
                piece += 2;
                break;
            }
            Some(b':') => {
                at += 1;
                if at == input.len() {
                    return None;
                }
            }
            Some(_) => return None,
            None => {}
        }
        pieces[piece] = value;
        piece += 1;
    }
    match compress {
        Some(compress) => pieces[compress..].rotate_right(8 - piece),
        None if piece != 8 => return None,
        None => {}
    }
    Some(pieces)
}

fn push_hex(out: &mut String, value: u16) {
    let mut shift = 12;
    while shift > 0 && value >> shift == 0 {
        shift -= 4;
    }
    loop {
        out.push(b"0123456789abcdef"[usize::from(value >> shift & 15)] as char);
        if shift == 0 {
            break;
        }
        shift -= 4;
    }
}

/// The first longest run of two or more zero pieces, as (start, length).
fn longest_zero_run(pieces: &[u16; 8]) -> (usize, usize) {
    let mut best = (8, 1);
    let mut i = 0;
    while i < 8 {
        let start = i;
        while i < 8 && pieces[i] == 0 {
            i += 1;
        }
        if i - start > best.1 {
            best = (start, i - start);
        }
        i += 1;
    }
    best
}

pub(crate) fn write_ipv6(out: &mut String, address: Ipv6Addr) {
    let pieces = address.segments();
    let (zeros, length) = longest_zero_run(&pieces);
    let mut i = 0;
    while i < 8 {
        if i == zeros {
            out.push_str(if i == 0 { "::" } else { ":" });
            i += length;
            continue;
        }
        push_hex(out, pieces[i]);
        if i < 7 {
            out.push(':');
        }
        i += 1;
    }
}
