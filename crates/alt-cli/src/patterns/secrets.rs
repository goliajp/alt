//! Credential shapes. Each function answers the text a leftmost search for
//! the pattern in its doc comment would match.

use super::boundary;

/// A credential shape, in the order the scan prefers when a line holds
/// several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    PrivateKey,
    CloudAccessKey,
    ForgeToken,
    SkApiKey,
    ChatToken,
    BrowserApiKey,
    PaymentLiveKey,
}

type Finder = fn(&str) -> Option<&str>;

const FINDERS: [(Shape, Finder); 7] = [
    (Shape::PrivateKey, private_key),
    (Shape::CloudAccessKey, cloud_access_key),
    (Shape::ForgeToken, forge_token),
    (Shape::SkApiKey, sk_api_key),
    (Shape::ChatToken, chat_token),
    (Shape::BrowserApiKey, browser_api_key),
    (Shape::PaymentLiveKey, payment_live_key),
];

/// The first shape `line` holds, in declaration order, and the text of it.
pub fn credential(line: &str) -> Option<(Shape, &str)> {
    let present = openers(line);
    if present == 0 {
        return None;
    }
    FINDERS
        .iter()
        .enumerate()
        .filter(|(i, _)| present & (1 << i) != 0)
        .find_map(|(_, (shape, find))| Some((*shape, find(line)?)))
}

/// The first two bytes of the literals each shape can open with, and the
/// bits of those shapes.
const OPENERS: [(&[u8; 2], u8); 9] = [
    (b"--", 1 << 0),
    (b"AK", 1 << 1),
    (b"AS", 1 << 1),
    (b"gh", 1 << 2),
    (b"gi", 1 << 2),
    (b"sk", 1 << 3 | 1 << 6),
    (b"xo", 1 << 4),
    (b"AI", 1 << 5),
    (b"rk", 1 << 6),
];

/// For each byte, the shapes it can be the first or the second byte of.
const fn opener_bytes(place: usize) -> [u8; 256] {
    let mut table = [0; 256];
    let mut i = 0;
    while i < OPENERS.len() {
        table[OPENERS[i].0[place] as usize] |= OPENERS[i].1;
        i += 1;
    }
    table
}

static FIRST: [u8; 256] = opener_bytes(0);
static SECOND: [u8; 256] = opener_bytes(1);

/// Bit `i` is set when `line` may hold the `i`th shape. Branch-free: this
/// is the one loop that runs over every byte scanned.
fn openers(line: &str) -> u8 {
    let (mut present, mut first) = (0, 0);
    for &b in line.as_bytes() {
        present |= first & SECOND[b as usize];
        first = FIRST[b as usize];
    }
    present
}

/// `-----BEGIN [A-Z ]*PRIVATE KEY( BLOCK)?-----`
pub fn private_key(line: &str) -> Option<&str> {
    const HEAD: &str = "-----BEGIN ";
    occurrences(line, HEAD).find_map(|at| {
        let body = &line[at + HEAD.len()..];
        let (label, tail) = body.split_at(run(body, |b| b.is_ascii_uppercase() || b == b' '));
        let named = label.ends_with("PRIVATE KEY") || label.ends_with("PRIVATE KEY BLOCK");
        (named && tail.starts_with("-----")).then(|| &line[at..at + HEAD.len() + label.len() + 5])
    })
}

/// `\b(AKIA|ASIA)[0-9A-Z]{16}\b`
pub fn cloud_access_key(line: &str) -> Option<&str> {
    let end = |from: usize| {
        let n = run(&line[from..], |b| {
            b.is_ascii_uppercase() || b.is_ascii_digit()
        });
        (n >= 16 && boundary(line, from + 16)).then_some(from + 16)
    };
    cut(
        line,
        earlier(first(line, "AKIA", end), first(line, "ASIA", end)),
    )
}

/// `\b(gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})\b`
pub fn forge_token(line: &str) -> Option<&str> {
    let short = first(line, "gh", |from| {
        let kind = line.as_bytes().get(from..from + 2)?;
        if !b"pousr".contains(&kind[0]) || kind[1] != b'_' {
            return None;
        }
        let end = from + 2 + at_least(&line[from + 2..], 36, |b| b.is_ascii_alphanumeric())?;
        boundary(line, end).then_some(end)
    });
    let fine_grained = first(line, "github_pat_", |from| {
        let end = from
            + at_least(&line[from..], 60, |b| {
                b.is_ascii_alphanumeric() || b == b'_'
            })?;
        boundary(line, end).then_some(end)
    });
    cut(line, earlier(short, fine_grained))
}

/// `\bsk-(proj-|ant-)?[A-Za-z0-9_-]{32,}`
pub fn sk_api_key(line: &str) -> Option<&str> {
    let found = first(line, "sk-", |from| {
        let body = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
        Some(from + at_least(&line[from..], 32, body)?)
    });
    cut(line, found)
}

/// `\bxox[baprs]-[A-Za-z0-9-]{10,}`
pub fn chat_token(line: &str) -> Option<&str> {
    let found = first(line, "xox", |from| {
        let kind = line.as_bytes().get(from..from + 2)?;
        if !b"baprs".contains(&kind[0]) || kind[1] != b'-' {
            return None;
        }
        let body = |b: u8| b.is_ascii_alphanumeric() || b == b'-';
        Some(from + 2 + at_least(&line[from + 2..], 10, body)?)
    });
    cut(line, found)
}

/// `\bAIza[0-9A-Za-z_-]{35}\b`
pub fn browser_api_key(line: &str) -> Option<&str> {
    let found = first(line, "AIza", |from| {
        let body = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
        at_least(&line[from..], 35, body)?;
        boundary(line, from + 35).then_some(from + 35)
    });
    cut(line, found)
}

/// `\b[sr]k_live_[0-9A-Za-z]{24,}\b`
pub fn payment_live_key(line: &str) -> Option<&str> {
    let end = |from: usize| {
        let end = from + at_least(&line[from..], 24, |b| b.is_ascii_alphanumeric())?;
        boundary(line, end).then_some(end)
    };
    cut(
        line,
        earlier(first(line, "sk_live_", end), first(line, "rk_live_", end)),
    )
}

const SECRET_NAMES: [&str; 13] = [
    "apikey",
    "api_key",
    "api-key",
    "secret",
    "token",
    "passwd",
    "password",
    "privatekey",
    "private_key",
    "private-key",
    "accesskey",
    "access_key",
    "access-key",
];

/// The third group of
/// `(?i)[a-z0-9_.-]*(api[_-]?key|secret|token|passw(or)?d|private[_-]?key|access[_-]?key)[a-z0-9_.-]*["']?\s*[:=]\s*["']?([A-Za-z0-9+/=_.-]{16,})`
pub fn assignment_value(line: &str) -> Option<&str> {
    let quote = ['"', '\''];
    let mut from = 0;
    // each separator has at most one name ending before it, so going
    // through the separators in order visits the names in order
    while let Some(i) = line.as_bytes()[from..]
        .iter()
        .position(|&b| b == b':' || b == b'=')
    {
        let sep = from + i;
        from = sep + 1;
        let tail = line[sep + 1..].trim_start();
        let tail = tail.strip_prefix(quote).unwrap_or(tail);
        let rest = tail.trim_start_matches(|c| is_name(c) || matches!(c, '+' | '/' | '='));
        let value = &tail[..tail.len() - rest.len()];
        if value.chars().count() < 16 {
            continue;
        }
        let head = line[..sep].trim_end();
        let head = head.strip_suffix(quote).unwrap_or(head);
        let name = &head[head.trim_end_matches(is_name).len()..];
        let folded: String = name.chars().map(fold).collect();
        if SECRET_NAMES.iter().any(|n| folded.contains(n)) {
            return Some(value);
        }
    }
    None
}

/// simple case folding, for the letters whose class includes an ascii one
fn fold(c: char) -> char {
    match c {
        '\u{17F}' => 's',
        '\u{212A}' => 'k',
        _ => c.to_ascii_lowercase(),
    }
}

fn is_name(c: char) -> bool {
    matches!(fold(c), 'a'..='z' | '0'..='9' | '_' | '.' | '-')
}

/// The length of the leading bytes of `s` that satisfy the ascii `class`.
fn run(s: &str, class: impl Fn(u8) -> bool) -> usize {
    s.bytes().position(|b| !class(b)).unwrap_or(s.len())
}

fn at_least(s: &str, min: usize, class: impl Fn(u8) -> bool) -> Option<usize> {
    let n = run(s, class);
    (n >= min).then_some(n)
}

/// Every byte offset where the ascii `lit` occurs in `s`, in order.
fn occurrences<'a>(s: &'a str, lit: &'a str) -> impl Iterator<Item = usize> + 'a {
    let (hay, needle) = (s.as_bytes(), lit.as_bytes());
    let mut from = 0;
    std::iter::from_fn(move || {
        while let Some(i) = hay[from..].iter().position(|&b| b == needle[0]) {
            let at = from + i;
            from = at + 1;
            if hay[at..].starts_with(needle) {
                return Some(at);
            }
        }
        None
    })
}

/// The first occurrence of `lit` that starts at a word boundary and that
/// `end`, given the offset after it, completes: its start and end.
fn first(line: &str, lit: &str, end: impl Fn(usize) -> Option<usize>) -> Option<(usize, usize)> {
    occurrences(line, lit)
        .filter(|&at| boundary(line, at))
        .find_map(|at| Some((at, end(at + lit.len())?)))
}

/// Whichever match starts first.
fn earlier(a: Option<(usize, usize)>, b: Option<(usize, usize)>) -> Option<(usize, usize)> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn cut(line: &str, found: Option<(usize, usize)>) -> Option<&str> {
    found.map(|(start, end)| &line[start..end])
}
