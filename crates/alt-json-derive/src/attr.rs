//! `#[json(...)]` attributes.

use proc_macro::{Delimiter, Span, TokenTree};

use crate::Error;
use crate::case::Rule;

#[derive(Default)]
pub(crate) struct Container {
    pub(crate) rename_all: Option<Rule>,
    pub(crate) deny_unknown_fields: bool,
    pub(crate) tag: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) try_from: Option<String>,
    pub(crate) into: Option<String>,
}

#[derive(Default)]
pub(crate) struct Field {
    pub(crate) rename: Option<String>,
    pub(crate) default: Option<Default>,
    pub(crate) skip_serializing_if: Option<String>,
    pub(crate) with: Option<String>,
}

pub(crate) enum Default {
    Trait,
    Path(String),
}

#[derive(Default)]
pub(crate) struct Variant {
    pub(crate) rename: Option<String>,
}

/// One `name` or `name = "value"` entry.
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) value: Option<String>,
    pub(crate) span: Span,
}

impl Entry {
    fn value(&self) -> Result<String, Error> {
        self.value.clone().ok_or_else(|| {
            Error::new(
                self.span,
                format!("`{}` needs a value: `{} = \"...\"`", self.name, self.name),
            )
        })
    }

    fn flag(&self) -> Result<(), Error> {
        match self.value {
            None => Ok(()),
            Some(_) => Err(Error::new(
                self.span,
                format!("`{}` takes no value", self.name),
            )),
        }
    }

    fn unknown(&self, place: &str, known: &str) -> Error {
        Error::new(
            self.span,
            format!(
                "unknown json attribute `{}` on a {place}, expected one of {known}",
                self.name
            ),
        )
    }
}

/// The entries of every `#[json(...)]` among `attributes`, each of which is
/// the bracketed part of one attribute. Other attributes are skipped.
pub(crate) fn entries(attributes: &[Vec<TokenTree>]) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::new();
    for attribute in attributes {
        let [TokenTree::Ident(name), rest @ ..] = attribute.as_slice() else {
            continue;
        };
        if name.to_string() != "json" {
            continue;
        }
        let [TokenTree::Group(list)] = rest else {
            return Err(Error::new(name.span(), "expected `#[json(...)]`"));
        };
        if list.delimiter() != Delimiter::Parenthesis {
            return Err(Error::new(list.span(), "expected `#[json(...)]`"));
        }
        let tokens: Vec<TokenTree> = list.stream().into_iter().collect();
        for entry in tokens.split(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ',')) {
            match entry {
                [] => {}
                [TokenTree::Ident(name)] => entries.push(Entry {
                    name: name.to_string(),
                    value: None,
                    span: name.span(),
                }),
                [
                    TokenTree::Ident(name),
                    TokenTree::Punct(eq),
                    TokenTree::Literal(value),
                ] if eq.as_char() == '=' => {
                    entries.push(Entry {
                        name: name.to_string(),
                        value: Some(string_literal(&value.to_string(), value.span())?),
                        span: name.span(),
                    });
                }
                [first, ..] => {
                    return Err(Error::new(
                        first.span(),
                        "expected `name` or `name = \"value\"`",
                    ));
                }
            }
        }
    }
    Ok(entries)
}

/// The text of a string literal; `\"` and `\\` are the only escapes read.
fn string_literal(text: &str, span: Span) -> Result<String, Error> {
    let malformed = || {
        Error::new(
            span,
            "expected a string literal; only the escapes \\\" and \\\\ are supported",
        )
    };
    let inner = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or_else(malformed)?;
    let mut value = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(escaped @ ('"' | '\\')) => value.push(escaped),
                _ => return Err(malformed()),
            },
            _ => value.push(c),
        }
    }
    Ok(value)
}

impl Container {
    pub(crate) fn parse(attributes: &[Vec<TokenTree>]) -> Result<Self, Error> {
        let mut container = Container::default();
        for entry in entries(attributes)? {
            match entry.name.as_str() {
                "rename_all" => {
                    container.rename_all = Some(Rule::parse(&entry.value()?, entry.span)?)
                }
                "deny_unknown_fields" => {
                    entry.flag()?;
                    container.deny_unknown_fields = true;
                }
                "tag" => container.tag = Some(entry.value()?),
                "content" => container.content = Some(entry.value()?),
                "try_from" => container.try_from = Some(entry.value()?),
                "into" => container.into = Some(entry.value()?),
                _ => {
                    return Err(entry.unknown(
                        "struct or enum",
                        "rename_all, deny_unknown_fields, tag, content, try_from, into",
                    ));
                }
            }
        }
        Ok(container)
    }
}

impl Field {
    pub(crate) fn parse(attributes: &[Vec<TokenTree>]) -> Result<Self, Error> {
        let mut field = Field::default();
        for entry in entries(attributes)? {
            match entry.name.as_str() {
                "rename" => field.rename = Some(entry.value()?),
                "default" => {
                    field.default = Some(match entry.value {
                        Some(path) => Default::Path(path),
                        None => Default::Trait,
                    });
                }
                "skip_serializing_if" => field.skip_serializing_if = Some(entry.value()?),
                "with" => field.with = Some(entry.value()?),
                _ => {
                    return Err(
                        entry.unknown("field", "rename, default, skip_serializing_if, with")
                    );
                }
            }
        }
        Ok(field)
    }
}

impl Variant {
    pub(crate) fn parse(attributes: &[Vec<TokenTree>]) -> Result<Self, Error> {
        let mut variant = Variant::default();
        for entry in entries(attributes)? {
            match entry.name.as_str() {
                "rename" => variant.rename = Some(entry.value()?),
                _ => return Err(entry.unknown("variant", "rename")),
            }
        }
        Ok(variant)
    }
}
