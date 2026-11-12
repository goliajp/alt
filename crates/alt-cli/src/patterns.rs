//! The fixed text shapes the checks look for: a conventional commit subject
//! and the forms credentials take. Each matcher answers what a leftmost
//! search for its pattern would.

mod secrets;
mod word;

pub use secrets::{
    Shape, assignment_value, browser_api_key, chat_token, cloud_access_key, credential,
    forge_token, payment_live_key, private_key, sk_api_key,
};

/// The parts of `type(scope)!: summary`.
#[derive(Debug, PartialEq, Eq)]
pub struct Conventional<'a> {
    pub kind: &'a str,
    pub breaking: bool,
    pub summary: &'a str,
}

/// `^([a-z]+)(\([^)]*\))?(!)?: (.+)$`
pub fn conventional(subject: &str) -> Option<Conventional<'_>> {
    let kind_len = subject
        .bytes()
        .position(|b| !b.is_ascii_lowercase())
        .unwrap_or(subject.len());
    if kind_len == 0 {
        return None;
    }
    let (kind, mut rest) = subject.split_at(kind_len);
    if let Some(scoped) = rest.strip_prefix('(')
        && let Some(close) = scoped.find(')')
    {
        rest = &scoped[close + 1..];
    }
    let bang = rest.strip_prefix('!');
    let summary = bang.unwrap_or(rest).strip_prefix(": ")?;
    (!summary.is_empty() && !summary.contains('\n')).then_some(Conventional {
        kind,
        breaking: bang.is_some(),
        summary,
    })
}

/// Whether `c` is a word character: alphabetic, a mark, a decimal digit,
/// connector punctuation or a join control.
fn is_word(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    word::BOUNDS.partition_point(|&b| b <= c as u32) % 2 == 1
}

/// Whether a word boundary lies at byte offset `at` of `s`.
fn boundary(s: &str, at: usize) -> bool {
    let before = s[..at].chars().next_back().is_some_and(is_word);
    let after = s[at..].chars().next().is_some_and(is_word);
    before != after
}
