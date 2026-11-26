//! The URLs of submodules Git refuses: one a transport would take for an
//! option, one that brings a line break into what a credential helper is
//! told, one whose `../` climbs past the host of the URL it is read against.

/// what Git reads as a separator of a path on any system
fn separator(byte: u8) -> bool {
    matches!(byte, b'/' | b'\\')
}

fn dot_slash(url: &[u8]) -> bool {
    url.len() >= 2 && url[0] == b'.' && separator(url[1])
}

fn dot_dot_slash(url: &[u8]) -> bool {
    url.len() >= 3 && url[..2] == *b".." && separator(url[2])
}

/// The value of a two-digit escape, where both digits are hexadecimal.
fn escaped(digits: &[u8]) -> Option<u8> {
    let digit = |byte: u8| (byte as char).to_digit(16);
    let (high, low) = (digit(*digits.first()?)?, digit(*digits.get(1)?)?);
    Some((high << 4 | low) as u8)
}

/// What Git decodes a URL to, its scheme as it is: an escape of NUL is left
/// as it is.
fn decoded(url: &[u8]) -> Vec<u8> {
    let from = url.iter().position(|byte| *byte == b':').unwrap_or(0);
    let mut out = url[..from].to_vec();
    let mut at = from;
    while at < url.len() {
        match escaped(&url[at + 1..]).filter(|_| url[at] == b'%') {
            Some(value) if value > 0 => {
                out.push(value);
                at += 3;
            }
            _ => {
                out.push(url[at]);
                at += 1;
            }
        }
    }
    out
}

/// Whether Git takes `url` as the URL of a submodule.
pub(crate) fn submodule_url_is_safe(url: &[u8]) -> bool {
    if url.starts_with(b"-") {
        return false;
    }
    if dot_slash(url) || dot_dot_slash(url) || url.starts_with(b"git://") {
        if decoded(url).contains(&b'\n') {
            return false;
        }
        let (mut rest, mut climbs) = (url, 0);
        loop {
            if dot_dot_slash(rest) {
                climbs += 1;
                rest = &rest[3..];
            } else if dot_slash(rest) {
                rest = &rest[2..];
            } else {
                break;
            }
        }
        return !(climbs > 0 && matches!(rest.first(), Some(b':' | b'/')));
    }
    let curl = [&b"http::"[..], b"https::", b"ftp::", b"ftps::"];
    let schemes = [&b"http://"[..], b"https://", b"ftp://", b"ftps://"];
    let sent = match curl.iter().find_map(|prefix| url.strip_prefix(*prefix)) {
        Some(rest) => rest,
        None if schemes.iter().any(|scheme| url.starts_with(scheme)) => url,
        None => return true,
    };
    normalizes(sent) && !decoded(sent).contains(&b'\n')
}

/// The bytes of `part` with its escapes read, where each is two hexadecimal
/// digits; none where one is not.
fn unescaped(part: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(part.len());
    let mut at = 0;
    while at < part.len() {
        if part[at] == b'%' {
            out.push(escaped(&part[at + 1..])?);
            at += 3;
        } else {
            out.push(part[at]);
            at += 1;
        }
    }
    Some(out)
}

/// Whether Git's normalizer of URLs takes `url`: a scheme and `://`, escapes
/// of two hexadecimal digits, a host of the characters a host has (none only
/// for `file:`), a port of 1 to 65535, and no `..` above the root of the path.
fn normalizes(url: &[u8]) -> bool {
    let scheme_byte = |byte: &u8| byte.is_ascii_alphanumeric() || b"+.-".contains(byte);
    let spanned = url.iter().take_while(|byte| scheme_byte(byte)).count();
    if spanned == 0
        || !url[0].is_ascii_alphabetic()
        || url.get(spanned..spanned + 3) != Some(b"://")
    {
        return false;
    }
    let scheme = url[..spanned].to_ascii_lowercase();
    let mut rest = &url[spanned + 3..];
    let ends = |rest: &[u8]| rest.iter().position(|byte| b"/?#".contains(byte));
    let slash = ends(rest).unwrap_or(rest.len());
    if let Some(at) = rest.iter().position(|byte| *byte == b'@')
        && at < slash
    {
        if unescaped(&rest[..at]).is_none() {
            return false;
        }
        rest = &rest[at + 1..];
    }
    let slash = ends(rest).unwrap_or(rest.len());
    let host = !rest.is_empty() && !b":/?#".contains(&rest[0]);
    if !host && scheme != b"file" {
        return false;
    }
    // the last colon before the path, where no `]` of an IPv6 host is after it
    let colon = match (1..slash).rev().find(|at| matches!(rest[*at], b':' | b']')) {
        _ if slash == 0 => slash,
        Some(at) if rest[at] == b':' => at,
        None if rest[0] == b':' => 0,
        _ => slash,
    };
    if !host && colon < slash && colon + 1 != slash {
        return false;
    }
    let host_byte = |byte: &u8| byte.is_ascii_alphanumeric() || b".-_[:]".contains(byte);
    if rest[..colon]
        .iter()
        .take_while(|byte| host_byte(byte))
        .count()
        < colon
    {
        return false;
    }
    if colon < slash {
        let digits = &rest[colon + 1..slash];
        let zeros = digits.iter().take_while(|byte| **byte == b'0').count();
        // zeros before it are dropped, and a port of zeros alone is 0
        let port = &digits[zeros.min(digits.len().saturating_sub(1))..];
        let default =
            (port == b"80" && scheme == b"http") || (port == b"443" && scheme == b"https");
        let value = (port.iter().all(u8::is_ascii_digit) && port.len() <= 5)
            .then(|| std::str::from_utf8(port).ok()?.parse::<u32>().ok())
            .flatten();
        if !port.is_empty() && !default && !value.is_some_and(|value| (1..=65535).contains(&value))
        {
            return false;
        }
    }
    rest = &rest[slash..];
    rest = rest.strip_prefix(b"/").unwrap_or(rest);
    let mut path: Vec<Vec<u8>> = Vec::new();
    loop {
        let end = ends(rest).unwrap_or(rest.len());
        let Some(segment) = unescaped(&rest[..end]) else {
            return false;
        };
        match &segment[..] {
            b"." => {}
            b".." if path.pop().is_none() => return false,
            b".." => {}
            _ => path.push(segment),
        }
        rest = &rest[end..];
        match rest.strip_prefix(b"/") {
            Some(after) => rest = after,
            None => break,
        }
    }
    unescaped(rest).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_taken_and_refused_as_git_takes_and_refuses_them() {
        let taken = [
            "../a.git",
            "./a",
            "..\\a",
            "git://host/a",
            "https://host/a.git",
            "https://user:pa%20ss@host:8443/a/../b",
            "http::https://host/a",
            "https://host:443/",
            "ssh://host/a",
            "host:a",
            "../%250a",
            "file:///a",
            // a URL no transport of curl takes is not normalized
            "file://:80/a",
        ];
        for url in taken {
            assert!(submodule_url_is_safe(url.as_bytes()), "{url}");
        }
        let refused = [
            "-x",
            "./a%0ab",
            "./a\nb",
            "../../:x",
            "..//x",
            "git://host/%0A",
            "https://host/a%0ab",
            "https:///a",
            "https::example.com/a",
            "https://ho%73t/a",
            "https://host:0/a",
            "https://host:65536/a",
            "https://host:8x/a",
            "https://host/..",
            "https://host/a/../..",
            "https://host/%2e%2e",
            "https://host/a%zz",
            "ftp://host/a%",
        ];
        for url in refused {
            assert!(!submodule_url_is_safe(url.as_bytes()), "{url}");
        }
    }
}
