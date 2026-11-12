//! `#[derive(Parser, Subcommand, Args, ValueEnum)]` for the `alt-args` traits.
//!
//! Supported shapes:
//!
//! - `Parser`: a struct with named fields, one of them `#[command(subcommand)]`
//! - `Args`: a struct with named fields
//! - `Subcommand`: an enum of unit variants, struct variants and one-field
//!   tuple variants holding an `Args` type
//! - `ValueEnum`: an enum of unit variants
//!
//! A doc comment becomes the help text. Attributes:
//!
//! - `#[command(...)]` on a `Parser` struct: `name = "..."`, `version`,
//!   `about = "..."`, `disable_help_subcommand = true`; on a variant:
//!   `name = "..."`; on a field: `subcommand`, `flatten`
//! - `#[arg(...)]` on a field: `long`, `long = "..."`, `short`, `short = 'c'`,
//!   `id = "..."`, `visible_alias = "..."`, `value_name = "..."`, `global = true`,
//!   `default_value = "..."`, `default_value_t = expr`, `required = true`,
//!   `action = set`, `value_parser = path`, `value_parser = ["a", "b"]`,
//!   `conflicts_with = "id"`, `conflicts_with_all = ["id", ...]`,
//!   `requires = "id"`, `required_unless_present = "id"`,
//!   `required_unless_present_any = ["id", ...]`, `group = "name"`,
//!   `trailing_var_arg = true`, `allow_hyphen_values = true`
//!
//! A field without `long` and `short` is positional. `bool` is a flag,
//! `Option<T>` an optional value, `Vec<T>` a repeatable one, and any other
//! type a single value that is required unless it has a default.

mod attr;
mod field;
mod generate;
mod parse;

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

fn expand(input: TokenStream, generate: fn(&parse::Input) -> Result<String, Error>) -> TokenStream {
    match parse::Input::parse(input).and_then(|input| generate(&input)) {
        Ok(code) => code.parse().unwrap_or_else(|_| {
            Error::new(Span::call_site(), "derive produced invalid code").into_tokens()
        }),
        Err(error) => error.into_tokens(),
    }
}

#[proc_macro_derive(Parser, attributes(command, arg))]
pub fn derive_parser(input: TokenStream) -> TokenStream {
    expand(input, generate::parser)
}

#[proc_macro_derive(Args, attributes(command, arg))]
pub fn derive_args(input: TokenStream) -> TokenStream {
    expand(input, generate::args)
}

#[proc_macro_derive(Subcommand, attributes(command, arg))]
pub fn derive_subcommand(input: TokenStream) -> TokenStream {
    expand(input, generate::subcommand)
}

#[proc_macro_derive(ValueEnum)]
pub fn derive_value_enum(input: TokenStream) -> TokenStream {
    expand(input, generate::value_enum)
}
