//! `#[arg(...)]` and `#[command(...)]` entries and doc comments.

use proc_macro::{Delimiter, Span, TokenStream, TokenTree};

use crate::Error;

/// One `name` or `name = value` entry.
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) value: Vec<TokenTree>,
    pub(crate) span: Span,
}

fn is_punct(token: &TokenTree, c: char) -> bool {
    matches!(token, TokenTree::Punct(p) if p.as_char() == c)
}

impl Entry {
    fn expected(&self, what: &str) -> Error {
        Error::new(self.span, format!("`{}` expects {what}", self.name))
    }

    /// `name` alone or `name = true`.
    pub(crate) fn enabled(&self) -> Result<bool, Error> {
        match self.value.as_slice() {
            [] => Ok(true),
            [TokenTree::Ident(word)] if word.to_string() == "true" => Ok(true),
            [TokenTree::Ident(word)] if word.to_string() == "false" => Ok(false),
            _ => Err(self.expected("`true` or `false`")),
        }
    }

    pub(crate) fn string(&self) -> Result<String, Error> {
        match self.value.as_slice() {
            [TokenTree::Literal(literal)] => {
                unquote(&literal.to_string()).ok_or_else(|| self.expected("a string literal"))
            }
            _ => Err(self.expected("a string literal")),
        }
    }

    pub(crate) fn character(&self) -> Result<char, Error> {
        let malformed = || self.expected("a character literal");
        let [TokenTree::Literal(literal)] = self.value.as_slice() else {
            return Err(malformed());
        };
        let text = literal.to_string();
        let inner = text
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .ok_or_else(malformed)?;
        let mut chars = inner.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c != '\\' => Ok(c),
            _ => Err(malformed()),
        }
    }

    /// `["a", "b"]`, or `None` when the value is not a bracketed list.
    pub(crate) fn list(&self) -> Result<Option<Vec<String>>, Error> {
        let [TokenTree::Group(group)] = self.value.as_slice() else {
            return Ok(None);
        };
        if group.delimiter() != Delimiter::Bracket {
            return Ok(None);
        }
        let mut items = Vec::new();
        for token in group.stream() {
            match &token {
                TokenTree::Literal(literal) => items.push(
                    unquote(&literal.to_string())
                        .ok_or_else(|| self.expected("string literals"))?,
                ),
                other if is_punct(other, ',') => {}
                _ => return Err(self.expected("string literals")),
            }
        }
        Ok(Some(items))
    }

    /// The value as source text.
    pub(crate) fn text(&self) -> Result<String, Error> {
        if self.value.is_empty() {
            return Err(self.expected("a value"));
        }
        Ok(self
            .value
            .iter()
            .cloned()
            .collect::<TokenStream>()
            .to_string())
    }
}

/// The entries of every `#[<kind>(...)]` among `attributes`, each of which
/// is the bracketed part of one attribute.
pub(crate) fn entries(attributes: &[Vec<TokenTree>], kind: &str) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::new();
    for attribute in attributes {
        let [TokenTree::Ident(name), TokenTree::Group(list)] = attribute.as_slice() else {
            continue;
        };
        if name.to_string() != kind || list.delimiter() != Delimiter::Parenthesis {
            continue;
        }
        let tokens: Vec<TokenTree> = list.stream().into_iter().collect();
        for entry in tokens.split(|t| is_punct(t, ',')) {
            match entry {
                [] => {}
                [TokenTree::Ident(name)] => entries.push(Entry {
                    name: name.to_string(),
                    value: Vec::new(),
                    span: name.span(),
                }),
                [TokenTree::Ident(name), eq, value @ ..] if is_punct(eq, '=') => {
                    entries.push(Entry {
                        name: name.to_string(),
                        value: value.to_vec(),
                        span: name.span(),
                    });
                }
                [first, ..] => {
                    return Err(Error::new(
                        first.span(),
                        "expected `name` or `name = value`",
                    ));
                }
            }
        }
    }
    Ok(entries)
}

/// The doc comment as one line of help: lines joined by a space, one
/// trailing period removed.
pub(crate) fn doc(attributes: &[Vec<TokenTree>]) -> Result<String, Error> {
    let mut lines = Vec::new();
    for attribute in attributes {
        let [TokenTree::Ident(name), eq, TokenTree::Literal(literal)] = attribute.as_slice() else {
            continue;
        };
        if name.to_string() != "doc" || !is_punct(eq, '=') {
            continue;
        }
        let text = unquote(&literal.to_string())
            .ok_or_else(|| Error::new(literal.span(), "unsupported doc comment literal"))?;
        if text.trim().is_empty() {
            return Err(Error::new(
                literal.span(),
                "help text of more than one paragraph is not supported",
            ));
        }
        lines.push(text.trim().to_owned());
    }
    let mut help = lines.join(" ");
    if help.ends_with('.') && !help.ends_with("..") {
        help.pop();
    }
    Ok(help)
}

/// The text of a string literal, plain or raw.
fn unquote(literal: &str) -> Option<String> {
    if let Some(raw) = literal.strip_prefix('r') {
        let hashes = raw.len() - raw.trim_start_matches('#').len();
        let inner = raw[hashes..].strip_prefix('"')?;
        let inner = inner[..inner.len().checked_sub(hashes)?].strip_suffix('"')?;
        return Some(inner.to_owned());
    }
    let inner = literal.strip_prefix('"')?.strip_suffix('"')?;
    let mut value = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            value.push(c);
            continue;
        }
        match chars.next()? {
            'n' => value.push('\n'),
            'r' => value.push('\r'),
            't' => value.push('\t'),
            'u' => {
                let digits: String = chars.by_ref().skip(1).take_while(|c| *c != '}').collect();
                value.push(char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?);
            }
            escaped @ ('"' | '\\' | '\'') => value.push(escaped),
            _ => return None,
        }
    }
    Some(value)
}
