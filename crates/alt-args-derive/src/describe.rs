//! The text of a description and what it names by number.

use proc_macro::Span;

use crate::Error;

pub(crate) fn quote(text: &str) -> String {
    format!("{text:?}")
}

/// A number below 128 as one byte of the description.
pub(crate) fn one(value: usize) -> String {
    char::from(
        u8::try_from(value)
            .ok()
            .filter(u8::is_ascii)
            .expect("a number below 128"),
    )
    .to_string()
}

pub(crate) fn two(value: usize) -> String {
    one(value / 128) + &one(value % 128)
}

pub(crate) fn three(value: usize) -> String {
    two(value / 128) + &one(value % 128)
}

/// A string behind its length in one byte.
pub(crate) fn short(text: &str, span: Span) -> Result<String, Error> {
    if text.len() >= 128 {
        return Err(Error::new(
            span,
            format!("`{text}` is longer than 127 bytes"),
        ));
    }
    Ok(one(text.len()) + text)
}

/// A string behind its length in two bytes.
pub(crate) fn long(text: &str, span: Span) -> Result<String, Error> {
    if text.len() >= 128 * 128 {
        return Err(Error::new(span, "a text is longer than 16383 bytes"));
    }
    Ok(two(text.len()) + text)
}

/// Words as one text, a line each.
pub(crate) fn lines(words: &[String], span: Span) -> Result<String, Error> {
    if words
        .iter()
        .any(|word| word.is_empty() || word.contains('\n'))
    {
        return Err(Error::new(
            span,
            "a listed word is empty or holds a line break",
        ));
    }
    long(&words.join("\n"), span)
}

/// What the description of a type names by number, gathered while its
/// text is written.
pub(crate) struct Named {
    pub(crate) span: Span,
    /// `number => expression,` for each argument whose values are checked
    pub(crate) checks: String,
    /// `number => expression,` for each computed default
    pub(crate) presets: String,
    /// for each argument, the expression of the values its type accepts
    pub(crate) typed: Vec<String>,
    /// the types of the groups spliced in and of the subcommands
    parts: Vec<String>,
}

impl Named {
    pub(crate) fn new(span: Span) -> Self {
        Named {
            span,
            checks: String::new(),
            presets: String::new(),
            typed: Vec::new(),
            parts: Vec::new(),
        }
    }

    pub(crate) fn part(&mut self, ty: &str) -> usize {
        self.parts.push(ty.to_owned());
        self.parts.len() - 1
    }

    /// The `Spec` of `ty` with `text` for its commands.
    pub(crate) fn spec(&self, ty: &str, text: &str, version: &str) -> String {
        let typed = match self.typed.iter().all(|piece| piece == "\"\"") {
            true => String::new(),
            false => format!(
                "const TYPED: &'static str = {{ \
                   const PIECES: &[&str] = &[{pieces}]; \
                   const LENGTH: usize = ::alt_args::joined_len(PIECES); \
                   const BYTES: [u8; LENGTH] = ::alt_args::joined(PIECES); \
                   match ::core::str::from_utf8(&BYTES) {{ \
                     ::core::result::Result::Ok(text) => text, \
                     ::core::result::Result::Err(_) => ::core::panic!() }} }};",
                pieces = self.typed.join(", ")
            ),
        };
        let parts: String = (self.parts.iter().enumerate())
            .map(|(number, ty)| format!("{number} => ::alt_args::Node::of::<{ty}>(), "))
            .collect();
        format!(
            "impl ::alt_args::Spec for {ty} {{ \
               const TEXT: &'static str = {text}; \
               const VERSION: &'static str = {version}; \
               {typed} \
               fn check(number: usize, value: &str) -> ::core::result::Result<(), ::std::string::String> {{ \
                 match number {{ {checks} _ => ::core::result::Result::Ok(()) }} }} \
               fn preset(number: usize) -> ::std::string::String {{ \
                 match number {{ {presets} _ => ::std::string::String::new() }} }} \
               fn part(number: usize) -> ::alt_args::Node {{ \
                 match number {{ {parts} _ => ::core::unreachable!() }} }} }}",
            text = quote(text),
            checks = self.checks,
            presets = self.presets,
        )
    }
}

pub(crate) fn items(items: &[String]) -> String {
    two(items.len()) + &items.concat()
}

/// One command of a description whose arguments are `items`.
pub(crate) fn command(
    help_subcommand: bool,
    subcommand: Option<usize>,
    about: &str,
    items: &str,
    span: Span,
) -> Result<String, Error> {
    Ok(one(usize::from(help_subcommand))
        + &two(subcommand.map_or(0, |part| part + 1))
        + &long(about, span)?
        + items)
}

/// The text of a description: where each command begins, then the
/// commands.
pub(crate) fn table(commands: &[(String, String)], span: Span) -> Result<String, Error> {
    let names: usize = commands.iter().map(|(name, _)| 1 + name.len() + 3).sum();
    let mut at = 2 + names;
    let mut text = two(commands.len());
    for (name, command) in commands {
        text += &(short(name, span)? + &three(at));
        at += command.len();
    }
    for (_, command) in commands {
        text += command;
    }
    Ok(text)
}
