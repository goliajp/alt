//! Generated trait implementations.

use crate::field::{ArgField, Member, Preset, Shape, kebab};
use crate::parse::{Data, Field, Input, Shape as VariantShape, text};
use crate::{Error, attr};

fn quote(text: &str) -> String {
    format!("{text:?}")
}

fn list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| quote(item)).collect();
    format!("&[{}]", quoted.join(", "))
}

/// The table entry of one argument.
fn arg_item(arg: &ArgField) -> String {
    let value = format!("<{} as ::alt_args::Value>", arg.value_type);
    let kind = match arg.shape {
        Shape::Flag => "Flag",
        Shape::Required | Shape::Optional => "Single",
        Shape::Repeated => "Multiple",
    };
    let preset = match &arg.preset {
        Preset::None => "::alt_args::Preset::None".to_owned(),
        Preset::Text(text) => format!("::alt_args::Preset::Text({})", quote(text)),
        // the binding gives an unsuffixed literal the type of the field
        Preset::Expression(expression) => format!(
            "::alt_args::Preset::Computed(|| {{ let value: {} = {expression}; ::std::string::ToString::to_string(&value) }})",
            arg.value_type
        ),
    };
    let (possible, check) = match (&arg.shape, &arg.possible, &arg.parser) {
        (Shape::Flag, _, _) => ("&[]".to_owned(), "::core::option::Option::None".to_owned()),
        (_, Some(possible), _) => (list(possible), "::core::option::Option::None".to_owned()),
        (_, None, Some(parser)) => (
            "&[]".to_owned(),
            format!(
                "::core::option::Option::Some(|text| {parser}(text).map(drop).map_err(|error| ::std::string::ToString::to_string(&error)))"
            ),
        ),
        (_, None, None) => (format!("{value}::POSSIBLE"), format!("{value}::CHECK")),
    };
    format!(
        "::alt_args::Item::Arg(::alt_args::Arg {{ id: {id}, long: {long}, short: {short}, alias: {alias}, \
         value_name: {value_name}, help: {help}, kind: ::alt_args::Kind::{kind}, required: {required}, \
         global: {global}, preset: {preset}, possible: {possible}, check: {check}, conflicts: {conflicts}, \
         requires: {requires}, required_unless: {required_unless}, group: {group}, trailing: {trailing}, \
         hyphen_values: {hyphen_values} }})",
        id = quote(&arg.id),
        long = quote(arg.long.as_deref().unwrap_or_default()),
        short = match arg.short {
            Some(short) => format!("::core::option::Option::Some({short:?})"),
            None => "::core::option::Option::None".to_owned(),
        },
        alias = quote(arg.alias.as_deref().unwrap_or_default()),
        value_name = quote(&arg.value_name),
        help = quote(&arg.help),
        required = arg.required,
        global = arg.global,
        conflicts = list(&arg.conflicts),
        requires = list(&arg.requires),
        required_unless = list(&arg.required_unless),
        group = quote(arg.group.as_deref().unwrap_or_default()),
        trailing = arg.trailing,
        hyphen_values = arg.hyphen_values,
    )
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
        Shape::Required => format!("m.required({id}, {parse})"),
        Shape::Optional => format!("m.optional({id}, {parse})"),
        Shape::Repeated => format!("m.repeated({id}, {parse})"),
    }
}

struct Members {
    /// the entries of the `Item` table
    items: Vec<String>,
    /// `field: value,` for each field
    values: String,
    /// the type of the subcommand field, if there is one
    subcommand: Option<String>,
    /// whether another type's arguments are spliced in
    flattened: bool,
}

fn members(fields: &[Field]) -> Result<Members, Error> {
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
                members
                    .items
                    .push("::alt_args::Item::Subcommands".to_owned());
                members.subcommand = Some(ty.clone());
                format!("<{ty} as ::alt_args::Subcommand>::from_matches(m.sub())")
            }
            Member::Flatten => {
                members.flattened = true;
                members
                    .items
                    .push(format!("<{ty} as ::alt_args::Args>::GROUP"));
                format!("<{ty} as ::alt_args::Args>::from_matches(m)")
            }
            Member::Arg(arg) => {
                members.items.push(arg_item(&arg));
                arg_value(&arg)
            }
        };
        members.values += &format!("{}: {value}, ", field.name);
    }
    Ok(members)
}

/// The arguments under the name of their struct or variant; a type that
/// splices another one in records its own arguments under no name.
fn group(name: &str, members: &Members) -> String {
    format!(
        "::alt_args::Item::Group({}, &[{}])",
        quote(if members.flattened { "" } else { name }),
        members.items.join(", ")
    )
}

fn subcommands(members: &Members) -> String {
    match &members.subcommand {
        Some(ty) => format!("<{ty} as ::alt_args::Subcommand>::COMMANDS"),
        None => "&[]".to_owned(),
    }
}

fn fields(input: &Input) -> Result<&[Field], Error> {
    match &input.data {
        Data::Struct(fields) => Ok(fields),
        Data::Enum(_) => Err(Error::new(input.span, "expected a struct")),
    }
}

pub(crate) fn parser(input: &Input) -> Result<String, Error> {
    let members = members(fields(input)?)?;
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
    Ok(format!(
        "impl ::alt_args::Parser for {ty} {{ \
           const COMMAND: &'static ::alt_args::Command = &::alt_args::Command {{ \
             name: {name}, about: {about}, version: {version}, help_subcommand: {help_subcommand}, \
             items: &[{group}], subcommands: {subcommands} }}; \
           fn from_matches(m: &::alt_args::Matches) -> Self {{ Self {{ {values} }} }} }}",
        ty = input.name,
        name = quote(&name),
        about = quote(&about),
        group = group(&input.name, &members),
        subcommands = subcommands(&members),
        values = members.values,
    ))
}

pub(crate) fn args(input: &Input) -> Result<String, Error> {
    let members = members(fields(input)?)?;
    Ok(format!(
        "impl ::alt_args::Args for {ty} {{ \
           const GROUP: ::alt_args::Item = {group}; \
           fn from_matches(m: &::alt_args::Matches) -> Self {{ Self {{ {values} }} }} }}",
        ty = input.name,
        group = group(&input.name, &members),
        values = members.values,
    ))
}

pub(crate) fn subcommand(input: &Input) -> Result<String, Error> {
    let Data::Enum(variants) = &input.data else {
        return Err(Error::new(input.span, "expected an enum"));
    };
    let mut commands = Vec::new();
    let mut arms = String::new();
    for variant in variants {
        let mut name = kebab(&variant.name);
        for entry in attr::entries(&variant.attributes, "command")? {
            match entry.name.as_str() {
                "name" => name = entry.string()?,
                other => {
                    return Err(Error::new(
                        entry.span,
                        format!("unknown command attribute `{other}` on a variant"),
                    ));
                }
            }
        }
        let (items, subcommands, value) = match &variant.shape {
            VariantShape::Unit => (String::new(), "&[]".to_owned(), String::new()),
            VariantShape::Named(fields) => {
                let members = members(fields)?;
                (
                    group(&variant.name, &members),
                    subcommands(&members),
                    format!(" {{ {} }}", members.values),
                )
            }
            VariantShape::Newtype(ty) => (
                format!("<{ty} as ::alt_args::Args>::GROUP"),
                "&[]".to_owned(),
                format!("(<{ty} as ::alt_args::Args>::from_matches(m))"),
            ),
        };
        commands.push(format!(
            "::alt_args::Command {{ name: {name}, about: {about}, version: \"\", help_subcommand: false, \
             items: &[{items}], subcommands: {subcommands} }}",
            name = quote(&name),
            about = quote(&attr::doc(&variant.attributes)?),
        ));
        arms += &format!("{} => Self::{}{value}, ", quote(&name), variant.name);
    }
    Ok(format!(
        "impl ::alt_args::Subcommand for {ty} {{ \
           const COMMANDS: &'static [::alt_args::Command] = &[{commands}]; \
           fn from_matches(m: &::alt_args::Matches) -> Self {{ \
             match m.name() {{ {arms} _ => ::core::unreachable!() }} }} }}",
        ty = input.name,
        commands = commands.join(", "),
    ))
}

pub(crate) fn value_enum(input: &Input) -> Result<String, Error> {
    let Data::Enum(variants) = &input.data else {
        return Err(Error::new(input.span, "expected an enum"));
    };
    let mut names = Vec::new();
    let mut arms = String::new();
    for variant in variants {
        if !matches!(variant.shape, VariantShape::Unit) {
            return Err(Error::new(variant.span, "expected a unit variant"));
        }
        let name = kebab(&variant.name);
        arms += &format!(
            "{} => ::core::result::Result::Ok(Self::{}), ",
            quote(&name),
            variant.name
        );
        names.push(name);
    }
    Ok(format!(
        "impl ::alt_args::Value for {ty} {{ \
           const POSSIBLE: &'static [&'static str] = {names}; \
           const CHECK: ::core::option::Option<::alt_args::Check> = ::core::option::Option::None; \
           fn parse(text: &str) -> ::core::result::Result<Self, ::std::string::String> {{ \
             match text {{ {arms} _ => ::core::result::Result::Err(::std::string::String::new()) }} }} }}",
        ty = input.name,
        names = list(&names),
    ))
}
