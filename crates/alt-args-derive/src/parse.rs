//! The derive input, read straight from the token stream.

use proc_macro::{Delimiter, Span, TokenStream, TokenTree};

use crate::Error;

pub(crate) struct Input {
    pub(crate) name: String,
    pub(crate) span: Span,
    /// the bracketed part of each attribute on the type
    pub(crate) attributes: Vec<Vec<TokenTree>>,
    pub(crate) data: Data,
}

pub(crate) enum Data {
    Struct(Vec<Field>),
    Enum(Vec<Variant>),
}

pub(crate) struct Field {
    pub(crate) name: String,
    pub(crate) span: Span,
    pub(crate) attributes: Vec<Vec<TokenTree>>,
    pub(crate) ty: Vec<TokenTree>,
}

pub(crate) struct Variant {
    pub(crate) name: String,
    pub(crate) span: Span,
    pub(crate) attributes: Vec<Vec<TokenTree>>,
    pub(crate) shape: Shape,
}

pub(crate) enum Shape {
    Unit,
    Named(Vec<Field>),
    /// the type of the only field
    Newtype(String),
}

struct Cursor {
    tokens: Vec<TokenTree>,
    at: usize,
    /// where to report running out of tokens
    end: Span,
}

fn is_punct(token: &TokenTree, c: char) -> bool {
    matches!(token, TokenTree::Punct(p) if p.as_char() == c)
}

pub(crate) fn text(tokens: &[TokenTree]) -> String {
    tokens.iter().cloned().collect::<TokenStream>().to_string()
}

impl Cursor {
    fn new(stream: TokenStream, end: Span) -> Self {
        Cursor {
            tokens: stream.into_iter().collect(),
            at: 0,
            end,
        }
    }

    fn peek(&self) -> Option<&TokenTree> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<TokenTree> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn ident(&mut self, what: &str) -> Result<(String, Span), Error> {
        match self.next() {
            Some(TokenTree::Ident(ident)) => Ok((ident.to_string(), ident.span())),
            Some(other) => Err(Error::new(other.span(), format!("expected {what}"))),
            None => Err(Error::new(self.end, format!("expected {what}"))),
        }
    }

    fn eat_punct(&mut self, c: char) -> bool {
        let found = self.peek().is_some_and(|t| is_punct(t, c));
        if found {
            self.at += 1;
        }
        found
    }

    /// The bracketed parts of the `#[...]` attributes at the cursor.
    fn attributes(&mut self) -> Vec<Vec<TokenTree>> {
        let mut attributes = Vec::new();
        while self.eat_punct('#') {
            if let Some(TokenTree::Group(group)) = self.next() {
                attributes.push(group.stream().into_iter().collect());
            }
        }
        attributes
    }

    fn visibility(&mut self) {
        if matches!(self.peek(), Some(TokenTree::Ident(i)) if i.to_string() == "pub") {
            self.at += 1;
            if matches!(self.peek(), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis)
            {
                self.at += 1;
            }
        }
    }

    /// Tokens up to a comma outside angle brackets, consuming the comma.
    fn until_comma(&mut self) -> Vec<TokenTree> {
        let mut depth = 0usize;
        let mut taken = Vec::new();
        while let Some(token) = self.next() {
            let after_minus = taken.last().is_some_and(|t| is_punct(t, '-'));
            if is_punct(&token, ',') && depth == 0 {
                break;
            } else if is_punct(&token, '<') {
                depth += 1;
            } else if is_punct(&token, '>') && !after_minus {
                depth = depth.saturating_sub(1);
            }
            taken.push(token);
        }
        taken
    }
}

impl Input {
    pub(crate) fn parse(stream: TokenStream) -> Result<Self, Error> {
        let mut cursor = Cursor::new(stream, Span::call_site());
        let attributes = cursor.attributes();
        cursor.visibility();
        let (keyword, keyword_span) = cursor.ident("`struct` or `enum`")?;
        let (name, span) = cursor.ident("a type name")?;
        let data = match (keyword.as_str(), cursor.next()) {
            ("struct", Some(TokenTree::Group(body))) if body.delimiter() == Delimiter::Brace => {
                Data::Struct(named_fields(body.stream(), body.span())?)
            }
            ("enum", Some(TokenTree::Group(body))) if body.delimiter() == Delimiter::Brace => {
                Data::Enum(variants(body.stream(), body.span())?)
            }
            _ => {
                return Err(Error::new(
                    keyword_span,
                    "expected a struct with named fields or an enum, without generics",
                ));
            }
        };
        Ok(Input {
            name,
            span,
            attributes,
            data,
        })
    }
}

fn named_fields(stream: TokenStream, span: Span) -> Result<Vec<Field>, Error> {
    let mut cursor = Cursor::new(stream, span);
    let mut fields = Vec::new();
    while cursor.peek().is_some() {
        let attributes = cursor.attributes();
        cursor.visibility();
        let (name, span) = cursor.ident("a field name")?;
        if !cursor.eat_punct(':') {
            return Err(Error::new(span, "expected `:` after the field name"));
        }
        fields.push(Field {
            name,
            span,
            attributes,
            ty: cursor.until_comma(),
        });
    }
    Ok(fields)
}

fn variants(stream: TokenStream, span: Span) -> Result<Vec<Variant>, Error> {
    let mut cursor = Cursor::new(stream, span);
    let mut variants = Vec::new();
    while cursor.peek().is_some() {
        let attributes = cursor.attributes();
        let (name, span) = cursor.ident("a variant name")?;
        let shape = match cursor.peek() {
            Some(TokenTree::Group(body)) if body.delimiter() == Delimiter::Brace => {
                Shape::Named(named_fields(body.stream(), body.span())?)
            }
            Some(TokenTree::Group(body)) if body.delimiter() == Delimiter::Parenthesis => {
                let mut inner = Cursor::new(body.stream(), body.span());
                let ty = inner.until_comma();
                if ty.is_empty() || inner.peek().is_some() {
                    return Err(Error::new(span, "a tuple variant takes exactly one field"));
                }
                Shape::Newtype(text(&ty))
            }
            _ => Shape::Unit,
        };
        cursor.until_comma();
        variants.push(Variant {
            name,
            span,
            attributes,
            shape,
        });
    }
    Ok(variants)
}
