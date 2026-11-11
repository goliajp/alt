//! `#[derive(ToJson, FromJson)]` for the `alt-json` traits.
//!
//! Supported shapes: structs with named fields, tuple structs, unit
//! structs, and enums with unit, single-field, tuple and struct variants,
//! with generic parameters and where clauses.
//!
//! Attributes, written `#[json(...)]`:
//!
//! - on a struct or enum: `rename_all = "..."`, `deny_unknown_fields`,
//!   `try_from = "Type"`, `into = "Type"`; on an enum also `tag = "..."`
//!   (the variant name becomes a member of the variant's own object) and
//!   `tag = "...", content = "..."` (name and data are two members)
//! - on a field: `rename = "..."`, `default`, `default = "path"`,
//!   `skip_serializing_if = "path"`, `with = "module"`
//! - on a variant: `rename = "..."`

mod attr;
mod case;
mod de;
mod parse;
mod ser;

use proc_macro::{Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream, TokenTree};

/// A problem with the input, reported at the token that caused it.
struct Error {
    message: String,
    span: Span,
}

impl Error {
    fn new(span: Span, message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
            span,
        }
    }

    /// `compile_error!("...")` spanned at the offending token.
    fn into_tokens(self) -> TokenStream {
        let mut message = Literal::string(&self.message);
        message.set_span(self.span);
        let mut bang = Punct::new('!', Spacing::Alone);
        bang.set_span(self.span);
        let mut arguments = Group::new(Delimiter::Parenthesis, TokenTree::from(message).into());
        arguments.set_span(self.span);
        let mut semicolon = Punct::new(';', Spacing::Alone);
        semicolon.set_span(self.span);
        [
            TokenTree::from(Ident::new("compile_error", self.span)),
            bang.into(),
            arguments.into(),
            semicolon.into(),
        ]
        .into_iter()
        .collect()
    }
}

fn expand(input: TokenStream, generate: fn(&parse::Input) -> String) -> TokenStream {
    match parse::Input::parse(input) {
        Ok(input) => generate(&input).parse().unwrap_or_else(|_| {
            Error::new(Span::call_site(), "derive produced invalid code").into_tokens()
        }),
        Err(error) => error.into_tokens(),
    }
}

#[proc_macro_derive(ToJson, attributes(json))]
pub fn derive_to_json(input: TokenStream) -> TokenStream {
    expand(input, ser::generate)
}

#[proc_macro_derive(FromJson, attributes(json))]
pub fn derive_from_json(input: TokenStream) -> TokenStream {
    expand(input, de::generate)
}
