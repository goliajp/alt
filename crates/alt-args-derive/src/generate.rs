//! Generated trait implementations.

use crate::field::{ArgField, Hint, Member, Preset, Shape, kebab};
use crate::parse::{Data, Field, Input, Shape as VariantShape, text};
use proc_macro::Span;

use crate::describe::{
    HELP_SUBCOMMAND, HIDDEN, Named, command, items, lines, long, one, quote, short, table, three,
    two,
};
use crate::{Error, attr};

/// One argument as the description has it.
fn arg_item(arg: &ArgField, named: &mut Named) -> Result<String, Error> {
    let span = named.span;
    let (number, ty) = (named.typed.len(), &arg.value_type);
    let kind = match arg.shape {
        Shape::Flag => 0,
        Shape::Count => 1,
        Shape::Attached => 2,
        Shape::Required | Shape::Optional => 3,
        Shape::Repeated => 4,
    } + 8 * usize::from(arg.last);
    let mut flags = usize::from(arg.required)
        + 2 * usize::from(arg.global)
        + 4 * usize::from(arg.trailing)
        + 8 * usize::from(arg.hyphen_values);
    let (mut listed, mut typed) = (Vec::new(), "\"\"".to_owned());
    match (&arg.shape, &arg.possible, &arg.parser) {
        (Shape::Flag | Shape::Count | Shape::Attached, _, _) => {}
        (_, Some(possible), _) => listed.clone_from(possible),
        (_, None, Some(parser)) => {
            flags += 16;
            named.checks += &format!(
                "{number} => {parser}(value).map(drop).map_err(|error| ::std::string::ToString::to_string(&error)), "
            );
        }
        (_, None, None) => {
            flags += 16 + 32;
            named.checks += &format!("{number} => ::alt_args::check_as::<{ty}>(value), ");
            typed = format!("<{ty} as ::alt_args::Value>::POSSIBLE");
        }
    }
    flags += 64 * usize::from(!listed.is_empty());
    named.typed.push(typed);
    let preset = match &arg.preset {
        Preset::None => one(0),
        Preset::Text(text) => one(1) + &long(text, span)?,
        // the binding gives an unsuffixed literal the type of the field
        Preset::Expression(expression) => {
            named.presets += &format!(
                "{number} => {{ let value: {} = {expression}; ::std::string::ToString::to_string(&value) }}, ",
                arg.value_type
            );
            one(2)
        }
    };
    let mask = usize::from(arg.alias.is_some())
        + 2 * usize::from(!matches!(arg.preset, Preset::None))
        + 4 * usize::from(arg.missing.is_some())
        + 8 * usize::from(!arg.conflicts.is_empty())
        + 16 * usize::from(!arg.requires.is_empty())
        + 32 * usize::from(!arg.required_unless.is_empty())
        + 64 * usize::from(arg.group.is_some());
    let rare = match mask {
        0 => String::new(),
        _ => {
            short(arg.alias.as_deref().unwrap_or_default(), span)?
                + &preset
                + &long(arg.missing.as_deref().unwrap_or_default(), span)?
                + &lines(&arg.conflicts, span)?
                + &lines(&arg.requires, span)?
                + &lines(&arg.required_unless, span)?
                + &short(arg.group.as_deref().unwrap_or_default(), span)?
        }
    };
    let hint = match &arg.hint {
        Hint::Unset => one(0),
        Hint::None => one(1),
        Hint::Path => one(2),
        Hint::Dir => one(3),
        Hint::Other(kind) => one(4) + &short(kind, span)?,
    };
    let rest =
        short(&arg.value_name, span)? + &long(&arg.help, span)? + &lines(&listed, span)? + &hint;
    let letter = arg.short.map(String::from).unwrap_or_default();
    Ok(one(0)
        + &one(kind)
        + &one(flags)
        + &one(mask)
        + &two(number)
        + &short(&letter, span)?
        + &short(&arg.id, span)?
        + &short(arg.long.as_deref().unwrap_or_default(), span)?
        + &long(&(rare + &rest), span)?)
}

/// The expression that reads one argument out of the matches `m`.
fn arg_value(arg: &ArgField) -> String {
    let id = quote(&arg.id);
    let parse = match &arg.parser {
        Some(parser) => parser.clone(),
        None => format!("<{} as ::alt_args::Value>::parse", arg.value_type),
    };
    match arg.shape {
        Shape::Flag => format!("m.flag({id})"),
        Shape::Count => format!("m.count({id}) as {}", arg.value_type),
        Shape::Attached => format!("m.all({id}).to_vec()"),
        Shape::Required => format!("m.required({id}, {parse})"),
        Shape::Optional => format!("m.optional({id}, {parse})"),
        Shape::Repeated => format!("m.repeated({id}, {parse})"),
    }
}

struct Members {
    /// the items of the description
    items: Vec<String>,
    /// `field: value,` for each field
    values: String,
    /// the part that describes the subcommands, if there is a field of them
    subcommand: Option<usize>,
    /// whether another type's arguments are spliced in
    flattened: bool,
}

fn members(fields: &[Field], named: &mut Named) -> Result<Members, Error> {
    let mut members = Members {
        items: Vec::new(),
        values: String::new(),
        subcommand: None,
        flattened: false,
    };
    for field in fields {
        let ty = text(&field.ty);
        let value = match Member::parse(field)? {
            Member::Subcommand => {
                members.items.push(one(3));
                members.subcommand = Some(named.part(&ty));
                format!("<{ty} as ::alt_args::Subcommand>::from_matches(m.sub())")
            }
            Member::Flatten => {
                members.flattened = true;
                members.items.push(one(2) + &two(named.part(&ty)));
                format!("<{ty} as ::alt_args::Args>::from_matches(m)")
            }
            Member::Arg(arg) => {
                members.items.push(arg_item(&arg, named)?);
                arg_value(&arg)
            }
        };
        members.values += &format!("{}: {value}, ", field.name);
    }
    Ok(members)
}

/// The arguments under the name of their struct or variant; a type that
/// splices another one in records its own arguments under no name.
fn group(name: &str, members: &Members, span: Span) -> Result<String, Error> {
    let inner = items(&members.items);
    let name = if members.flattened { "" } else { name };
    Ok(items(&[one(1)
        + &short(name, span)?
        + &three(inner.len())
        + &inner]))
}

fn fields(input: &Input) -> Result<&[Field], Error> {
    match &input.data {
        Data::Struct(fields) => Ok(fields),
        Data::Enum(_) => Err(Error::new(input.span, "expected a struct")),
    }
}

pub(crate) fn parser(input: &Input) -> Result<String, Error> {
    let mut named = Named::new(input.span);
    let members = members(fields(input)?, &mut named)?;
    let mut name = kebab(&input.name);
    let mut about = attr::doc(&input.attributes)?;
    let mut version = "\"\"";
    let mut help_subcommand = true;
    for entry in attr::entries(&input.attributes, "command")? {
        match entry.name.as_str() {
            "name" => name = entry.string()?,
            "about" => about = entry.string()?,
            "version" if entry.enabled()? => version = "::core::env!(\"CARGO_PKG_VERSION\")",
            "version" => {}
            "disable_help_subcommand" => help_subcommand = !entry.enabled()?,
            other => {
                return Err(Error::new(
                    entry.span,
                    format!("unknown command attribute `{other}` on a struct"),
                ));
            }
        }
    }
    let group = group(&input.name, &members, input.span)?;
    let command = command(
        HELP_SUBCOMMAND * usize::from(help_subcommand),
        members.subcommand,
        &about,
        &group,
        input.span,
    )?;
    Ok(format!(
        "{spec} impl ::alt_args::Parser for {ty} {{ \
           fn from_matches(m: &::alt_args::Matches) -> Self {{ Self {{ {values} }} }} }}",
        spec = named.spec(
            &input.name,
            &table(&[(name, command)], input.span)?,
            version
        ),
        ty = input.name,
        values = members.values,
    ))
}

pub(crate) fn args(input: &Input) -> Result<String, Error> {
    let mut named = Named::new(input.span);
    let members = members(fields(input)?, &mut named)?;
    let group = group(&input.name, &members, input.span)?;
    let command = command(0, None, "", &group, input.span)?;
    Ok(format!(
        "{spec} impl ::alt_args::Args for {ty} {{ \
           fn from_matches(m: &::alt_args::Matches) -> Self {{ Self {{ {values} }} }} }}",
        spec = named.spec(
            &input.name,
            &table(&[(String::new(), command)], input.span)?,
            "\"\""
        ),
        ty = input.name,
        values = members.values,
    ))
}

pub(crate) fn subcommand(input: &Input) -> Result<String, Error> {
    let Data::Enum(variants) = &input.data else {
        return Err(Error::new(input.span, "expected an enum"));
    };
    let mut named = Named::new(input.span);
    let mut commands = Vec::new();
    let mut arms = String::new();
    for variant in variants {
        let mut name = kebab(&variant.name);
        let mut hidden = false;
        for entry in attr::entries(&variant.attributes, "command")? {
            match entry.name.as_str() {
                "name" => name = entry.string()?,
                "hide" => hidden = entry.enabled()?,
                other => {
                    return Err(Error::new(
                        entry.span,
                        format!("unknown command attribute `{other}` on a variant"),
                    ));
                }
            }
        }
        let (items, subcommand, value) = match &variant.shape {
            VariantShape::Unit => (items(&[]), None, String::new()),
            VariantShape::Named(fields) => {
                let members = members(fields, &mut named)?;
                (
                    group(&variant.name, &members, variant.span)?,
                    members.subcommand,
                    format!(" {{ {} }}", members.values),
                )
            }
            VariantShape::Newtype(ty) => (
                items(&[one(2) + &two(named.part(ty))]),
                None,
                format!("(<{ty} as ::alt_args::Args>::from_matches(m))"),
            ),
        };
        let about = attr::doc(&variant.attributes)?;
        let flags = HIDDEN * usize::from(hidden);
        let command = command(flags, subcommand, &about, &items, variant.span)?;
        arms += &format!("{} => Self::{}{value}, ", quote(&name), variant.name);
        commands.push((name, command));
    }
    Ok(format!(
        "{spec} impl ::alt_args::Subcommand for {ty} {{ \
           fn from_matches(m: &::alt_args::Matches) -> Self {{ \
             match m.name() {{ {arms} _ => ::core::unreachable!() }} }} }}",
        spec = named.spec(&input.name, &table(&commands, input.span)?, "\"\""),
        ty = input.name,
    ))
}
