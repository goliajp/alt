//! The derive input, read straight from the token stream.

use proc_macro::{Delimiter, Span, TokenStream, TokenTree};

use crate::{Error, attr};

pub(crate) struct Input {
    pub(crate) name: String,
    pub(crate) generics: Vec<Param>,
    pub(crate) where_clause: String,
    pub(crate) attrs: attr::Container,
    pub(crate) data: Data,
}

pub(crate) enum Data {
    Struct(Fields),
    Enum(Vec<Variant>),
}

pub(crate) enum Fields {
    Named(Vec<Field>),
    Tuple(Vec<Field>),
    Unit,
}

pub(crate) struct Field {
    /// the identifier as written, `r#` included; the position for tuple fields
    pub(crate) member: String,
    /// position among the fields of its struct or variant
    pub(crate) index: usize,
    pub(crate) attrs: attr::Field,
}

pub(crate) struct Variant {
    pub(crate) name: String,
    pub(crate) span: Span,
    pub(crate) attrs: attr::Variant,
    pub(crate) fields: Fields,
}

pub(crate) enum ParamKind {
    Lifetime,
    Type,
    Const,
}

pub(crate) struct Param {
    pub(crate) kind: ParamKind,
    pub(crate) name: String,
    /// the declaration without its default
    pub(crate) declaration: String,
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

fn text(tokens: &[TokenTree]) -> String {
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

    fn span(&self) -> Span {
        self.peek().map_or(self.end, TokenTree::span)
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

    /// The parameters between `<` and its matching `>`, if the cursor is at
    /// a `<`.
    fn generics(&mut self) -> Result<Vec<Param>, Error> {
        if !self.eat_punct('<') {
            return Ok(Vec::new());
        }
        let mut depth = 1usize;
        let mut inner = Vec::new();
        loop {
            let Some(token) = self.next() else {
                return Err(Error::new(self.end, "unclosed generic parameter list"));
            };
            let after_minus = inner.last().is_some_and(|t| is_punct(t, '-'));
            if is_punct(&token, '<') {
                depth += 1;
            } else if is_punct(&token, '>') && !after_minus {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            inner.push(token);
        }
        let mut params = Vec::new();
        let mut cursor = Cursor {
            tokens: inner,
            at: 0,
            end: self.end,
        };
        while cursor.peek().is_some() {
            let tokens = cursor.until_comma();
            if !tokens.is_empty() {
                params.push(Param::parse(&tokens)?);
            }
        }
        Ok(params)
    }

    /// A where clause, up to the body or the closing semicolon.
    fn where_clause(&mut self) -> String {
        let mut clause = Vec::new();
        if matches!(self.peek(), Some(TokenTree::Ident(i)) if i.to_string() == "where") {
            while let Some(token) = self.peek() {
                let body =
                    matches!(token, TokenTree::Group(g) if g.delimiter() == Delimiter::Brace);
                if body || is_punct(token, ';') {
                    break;
                }
                clause.extend(self.next());
            }
        }
        text(&clause)
    }
}

impl Param {
    fn parse(tokens: &[TokenTree]) -> Result<Self, Error> {
        // a default starts at the first `=` outside angle brackets
        let mut depth = 0usize;
        let default = tokens.iter().position(|token| {
            if is_punct(token, '<') {
                depth += 1;
            } else if is_punct(token, '>') {
                depth = depth.saturating_sub(1);
            }
            depth == 0 && is_punct(token, '=')
        });
        let declaration = text(&tokens[..default.unwrap_or(tokens.len())]);
        let (kind, name) = match tokens {
            [TokenTree::Punct(tick), TokenTree::Ident(name), ..] if tick.as_char() == '\'' => {
                (ParamKind::Lifetime, format!("'{name}"))
            }
            [TokenTree::Ident(keyword), TokenTree::Ident(name), ..]
                if keyword.to_string() == "const" =>
            {
                (ParamKind::Const, name.to_string())
            }
            [TokenTree::Ident(name), ..] => (ParamKind::Type, name.to_string()),
            [first, ..] => return Err(Error::new(first.span(), "expected a generic parameter")),
            [] => {
                return Err(Error::new(
                    Span::call_site(),
                    "expected a generic parameter",
                ));
            }
        };
        Ok(Param {
            kind,
            name,
            declaration,
        })
    }
}

impl Input {
    pub(crate) fn parse(stream: TokenStream) -> Result<Self, Error> {
        let mut cursor = Cursor::new(stream, Span::call_site());
        let attrs = attr::Container::parse(&cursor.attributes())?;
        cursor.visibility();
        let (keyword, keyword_span) = cursor.ident("`struct` or `enum`")?;
        let (name, _) = cursor.ident("a type name")?;
        let generics = cursor.generics()?;
        let mut where_clause = cursor.where_clause();
        let data = match (keyword.as_str(), cursor.next()) {
            ("struct", Some(TokenTree::Group(body))) if body.delimiter() == Delimiter::Brace => {
                Data::Struct(Fields::Named(named_fields(body.stream(), body.span())?))
            }
            ("struct", Some(TokenTree::Group(body)))
                if body.delimiter() == Delimiter::Parenthesis =>
            {
                where_clause = cursor.where_clause();
                Data::Struct(Fields::Tuple(tuple_fields(body.stream(), body.span())?))
            }
            ("struct", Some(token)) if is_punct(&token, ';') => Data::Struct(Fields::Unit),
            ("enum", Some(TokenTree::Group(body))) if body.delimiter() == Delimiter::Brace => {
                Data::Enum(variants(body.stream(), body.span())?)
            }
            ("struct" | "enum", other) => {
                let span = other.map_or(keyword_span, |t| t.span());
                return Err(Error::new(span, "expected the body of the type"));
            }
            _ => {
                return Err(Error::new(
                    keyword_span,
                    "only structs and enums can derive this",
                ));
            }
        };
        let input = Input {
            name,
            generics,
            where_clause,
            attrs,
            data,
        };
        input.check(keyword_span)?;
        Ok(input)
    }

    /// Rejects attribute combinations that have no meaning.
    fn check(&self, keyword: Span) -> Result<(), Error> {
        let tagged = self.attrs.tag.is_some();
        if self.attrs.content.is_some() && !tagged {
            return Err(Error::new(keyword, "`content` needs `tag`"));
        }
        let Data::Enum(variants) = &self.data else {
            return match tagged {
                true => Err(Error::new(keyword, "`tag` is only supported on enums")),
                false => Ok(()),
            };
        };
        if tagged && self.attrs.content.is_none() {
            for variant in variants {
                if matches!(&variant.fields, Fields::Tuple(fields) if fields.len() != 1) {
                    return Err(Error::new(
                        variant.span,
                        "a tuple variant cannot carry an internal tag; use a struct variant or add `content`",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn named_fields(stream: TokenStream, span: Span) -> Result<Vec<Field>, Error> {
    let mut cursor = Cursor::new(stream, span);
    let mut fields = Vec::new();
    while cursor.peek().is_some() {
        let attrs = attr::Field::parse(&cursor.attributes())?;
        cursor.visibility();
        let (member, _) = cursor.ident("a field name")?;
        if !cursor.eat_punct(':') {
            return Err(Error::new(
                cursor.span(),
                "expected `:` after the field name",
            ));
        }
        cursor.until_comma();
        fields.push(Field {
            member,
            index: fields.len(),
            attrs,
        });
    }
    Ok(fields)
}

fn tuple_fields(stream: TokenStream, span: Span) -> Result<Vec<Field>, Error> {
    let mut cursor = Cursor::new(stream, span);
    let mut fields = Vec::new();
    while cursor.peek().is_some() {
        let attrs = attr::Field::parse(&cursor.attributes())?;
        cursor.visibility();
        if cursor.until_comma().is_empty() {
            return Err(Error::new(cursor.span(), "expected a field type"));
        }
        fields.push(Field {
            member: fields.len().to_string(),
            index: fields.len(),
            attrs,
        });
    }
    Ok(fields)
}

fn variants(stream: TokenStream, span: Span) -> Result<Vec<Variant>, Error> {
    let mut cursor = Cursor::new(stream, span);
    let mut variants = Vec::new();
    while cursor.peek().is_some() {
        let attrs = attr::Variant::parse(&cursor.attributes())?;
        let (name, span) = cursor.ident("a variant name")?;
        let fields = match cursor.peek() {
            Some(TokenTree::Group(body)) if body.delimiter() == Delimiter::Brace => {
                Fields::Named(named_fields(body.stream(), body.span())?)
            }
            Some(TokenTree::Group(body)) if body.delimiter() == Delimiter::Parenthesis => {
                Fields::Tuple(tuple_fields(body.stream(), body.span())?)
            }
            _ => Fields::Unit,
        };
        // also skips an explicit discriminant
        cursor.until_comma();
        variants.push(Variant {
            name,
            span,
            attrs,
            fields,
        });
    }
    Ok(variants)
}

impl Input {
    /// `impl<...> ::alt_json::Trait for Name<...> where ...`, with the trait
    /// required of every type parameter.
    pub(crate) fn impl_header(&self, trait_name: &str) -> String {
        let mut declarations = Vec::new();
        let mut names = Vec::new();
        for param in &self.generics {
            names.push(param.name.clone());
            declarations.push(match param.kind {
                ParamKind::Type if param.declaration.contains(':') => {
                    format!("{} + ::alt_json::{trait_name}", param.declaration)
                }
                ParamKind::Type => format!("{}: ::alt_json::{trait_name}", param.declaration),
                _ => param.declaration.clone(),
            });
        }
        format!(
            "impl<{}> ::alt_json::{trait_name} for {}<{}> {}",
            declarations.join(", "),
            self.name,
            names.join(", "),
            self.where_clause
        )
    }

    /// The name a variant has in JSON.
    pub(crate) fn variant_name(&self, variant: &Variant) -> String {
        match (&variant.attrs.rename, self.attrs.rename_all) {
            (Some(name), _) => name.clone(),
            (None, Some(rule)) => rule.variant(&variant.name),
            (None, None) => variant.name.clone(),
        }
    }

    /// The name a field has in JSON. Only a struct's own `rename_all`
    /// reaches its fields; an enum's applies to its variants.
    pub(crate) fn field_name(&self, field: &Field) -> String {
        let plain = field.member.trim_start_matches("r#");
        let rule = match self.data {
            Data::Struct(_) => self.attrs.rename_all,
            Data::Enum(_) => None,
        };
        match (&field.attrs.rename, rule) {
            (Some(name), _) => name.clone(),
            (None, Some(rule)) => rule.field(plain),
            (None, None) => plain.to_owned(),
        }
    }
}

/// A Rust string literal for `text`.
pub(crate) fn quote(text: &str) -> String {
    format!("{text:?}")
}
