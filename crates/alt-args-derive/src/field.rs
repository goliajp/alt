//! What one struct field or variant field means on the command line.

use proc_macro::TokenTree;

use crate::parse::{Field, text};
use crate::{Error, attr};

pub(crate) enum Member {
    Subcommand,
    Flatten,
    Arg(Box<ArgField>),
}

pub(crate) enum Shape {
    Flag,
    /// a flag counted into an integer field
    Count,
    /// a flag given bare or with a value against its letter, each
    /// appearance kept as text
    Attached,
    Required,
    Optional,
    Repeated,
}

/// What kind of thing a value is, as `#[arg(hint = ...)]` says it.
pub(crate) enum Hint {
    Unset,
    None,
    Path,
    Dir,
    Other(String),
}

pub(crate) enum Preset {
    None,
    Text(String),
    Expression(String),
}

pub(crate) struct ArgField {
    pub(crate) id: String,
    pub(crate) long: Option<String>,
    pub(crate) short: Option<char>,
    pub(crate) alias: Option<String>,
    pub(crate) value_name: String,
    pub(crate) help: String,
    pub(crate) shape: Shape,
    /// the value type: the field type with `Option` or `Vec` removed
    pub(crate) value_type: String,
    pub(crate) global: bool,
    pub(crate) required: bool,
    pub(crate) preset: Preset,
    pub(crate) missing: Option<String>,
    pub(crate) parser: Option<String>,
    pub(crate) possible: Option<Vec<String>>,
    pub(crate) conflicts: Vec<String>,
    pub(crate) requires: Vec<String>,
    pub(crate) required_unless: Vec<String>,
    pub(crate) group: Option<String>,
    pub(crate) trailing: bool,
    /// takes the words after `--` and no other
    pub(crate) last: bool,
    pub(crate) hyphen_values: bool,
    pub(crate) hint: Hint,
}

/// `inner` when `ty` is `<wrapper><inner>`.
fn unwrap<'a>(ty: &'a [TokenTree], wrapper: &str) -> Option<&'a [TokenTree]> {
    match ty {
        [
            TokenTree::Ident(name),
            TokenTree::Punct(open),
            inner @ ..,
            TokenTree::Punct(close),
        ] if name.to_string() == wrapper && open.as_char() == '<' && close.as_char() == '>' => {
            Some(inner)
        }
        _ => None,
    }
}

pub(crate) fn kebab(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() && !out.is_empty() {
            out.push('-');
        }
        out.push(if c == '_' {
            '-'
        } else {
            c.to_ascii_lowercase()
        });
    }
    out
}

fn one_or_list(entry: &attr::Entry) -> Result<Vec<String>, Error> {
    match entry.list()? {
        Some(list) => Ok(list),
        None => Ok(vec![entry.string()?]),
    }
}

/// `none`, `path`, `dir`, or a string naming a kind of the program's own.
fn hint(entry: &attr::Entry) -> Result<Hint, Error> {
    Ok(match entry.text()?.as_str() {
        "none" => Hint::None,
        "path" => Hint::Path,
        "dir" => Hint::Dir,
        _ => Hint::Other(entry.string()?),
    })
}

impl Member {
    pub(crate) fn parse(field: &Field) -> Result<Self, Error> {
        match attr::entries(&field.attributes, "command")?.first() {
            None => Ok(Member::Arg(Box::new(ArgField::parse(field)?))),
            Some(entry) if entry.name == "subcommand" => Ok(Member::Subcommand),
            Some(entry) if entry.name == "flatten" => Ok(Member::Flatten),
            Some(entry) => Err(Error::new(
                entry.span,
                format!("unknown command attribute `{}` on a field", entry.name),
            )),
        }
    }
}

/// How many values a field of the type `ty` takes, and the type of one.
fn shape_of(ty: &[TokenTree]) -> (Shape, String) {
    if text(ty) == "bool" {
        (Shape::Flag, text(ty))
    } else if let Some(inner) = unwrap(ty, "Option") {
        (Shape::Optional, text(inner))
    } else if let Some(inner) = unwrap(ty, "Vec") {
        (Shape::Repeated, text(inner))
    } else {
        (Shape::Required, text(ty))
    }
}

impl ArgField {
    fn parse(field: &Field) -> Result<Self, Error> {
        let name = field.name.trim_start_matches("r#");
        let (mut shape, value_type) = shape_of(&field.ty);
        let mut arg = ArgField {
            id: name.to_owned(),
            long: None,
            short: None,
            alias: None,
            value_name: name.to_ascii_uppercase(),
            help: attr::doc(&field.attributes)?,
            shape: Shape::Flag,
            value_type,
            global: false,
            required: false,
            preset: Preset::None,
            missing: None,
            parser: None,
            possible: None,
            conflicts: Vec::new(),
            requires: Vec::new(),
            required_unless: Vec::new(),
            group: None,
            trailing: false,
            last: false,
            hyphen_values: false,
            hint: Hint::Unset,
        };
        for entry in attr::entries(&field.attributes, "arg")? {
            match entry.name.as_str() {
                "long" if entry.value.is_empty() => arg.long = Some(kebab(name)),
                "long" => arg.long = Some(entry.string()?),
                "short" if entry.value.is_empty() => arg.short = name.chars().next(),
                "short" => arg.short = Some(entry.character()?),
                "id" => arg.id = entry.string()?,
                "visible_alias" => arg.alias = Some(entry.string()?),
                "value_name" => arg.value_name = entry.string()?,
                "global" => arg.global = entry.enabled()?,
                "required" => arg.required = entry.enabled()?,
                "default_value" => arg.preset = Preset::Text(entry.string()?),
                "default_value_t" => arg.preset = Preset::Expression(entry.text()?),
                "default_missing_value" => arg.missing = Some(entry.string()?),
                "action" if entry.text()? == "set" => shape = Shape::Required,
                "action" if entry.text()? == "count" => shape = Shape::Count,
                "action" if entry.text()? == "attached" => shape = Shape::Attached,
                "action" => {
                    let known = "the actions are `set`, `count` and `attached`";
                    return Err(Error::new(entry.span, known));
                }
                "value_parser" => match entry.list()? {
                    Some(list) => arg.possible = Some(list),
                    None => arg.parser = Some(entry.text()?),
                },
                "conflicts_with" | "conflicts_with_all" => arg.conflicts = one_or_list(&entry)?,
                "requires" => arg.requires = one_or_list(&entry)?,
                "required_unless_present" | "required_unless_present_any" => {
                    arg.required_unless = one_or_list(&entry)?
                }
                "group" => arg.group = Some(entry.string()?),
                "trailing_var_arg" => arg.trailing = entry.enabled()?,
                "last" => arg.last = entry.enabled()?,
                "allow_hyphen_values" => arg.hyphen_values = entry.enabled()?,
                "hint" => arg.hint = hint(&entry)?,
                other => {
                    return Err(Error::new(
                        entry.span,
                        format!("unknown arg attribute `{other}`"),
                    ));
                }
            }
        }
        if matches!(shape, Shape::Required) && matches!(arg.preset, Preset::None) {
            arg.required = true;
        }
        let flag = matches!(shape, Shape::Flag | Shape::Count | Shape::Attached);
        if flag && arg.long.is_none() && arg.short.is_none() {
            return Err(Error::new(field.span, "a flag needs `long` or `short`"));
        }
        arg.shape = shape;
        Ok(arg)
    }
}
