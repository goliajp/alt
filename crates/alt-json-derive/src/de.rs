//! Generated `FromJson` implementations.

use crate::attr::Default;
use crate::parse::{Data, Field, Fields, Input, Variant, quote};

const OK: &str = "::core::result::Result::Ok";
const ERR: &str = "::core::result::Result::Err";
const SOME: &str = "::core::option::Option::Some";
const NONE: &str = "::core::option::Option::None";

pub(crate) fn generate(input: &Input) -> String {
    let (body, key_body) = match (&input.attrs.try_from, &input.data) {
        (Some(source), _) => {
            let convert = format!(
                "<Self as ::core::convert::TryFrom<{source}>>::try_from(value).map_err(::alt_json::Error::custom)"
            );
            (
                format!("let value = <{source} as ::alt_json::FromJson>::from_json(d)?; {convert}"),
                Some(format!(
                    "let value = <{source} as ::alt_json::FromJson>::from_json_key(d)?; {convert}"
                )),
            )
        }
        (None, Data::Struct(fields)) => struct_body(input, fields),
        (None, Data::Enum(variants)) => enum_body(input, variants),
    };
    let key_method = key_body.map_or(String::new(), |key_body| {
        format!("fn from_json_key(d: &mut ::alt_json::Decoder<'_>) -> ::alt_json::Result<Self> {{ {key_body} }}")
    });
    format!(
        "{} {{ fn from_json(d: &mut ::alt_json::Decoder<'_>) -> ::alt_json::Result<Self> {{ {body} }} {key_method} }}",
        input.impl_header("FromJson")
    )
}

/// The function that reads one field.
fn reader(field: &Field) -> String {
    match &field.attrs.with {
        Some(module) => format!("{module}::from_json"),
        None => "::alt_json::FromJson::from_json".to_owned(),
    }
}

fn default(field: &Field) -> Option<String> {
    Some(match field.attrs.default.as_ref()? {
        Default::Trait => "::core::default::Default::default()".to_owned(),
        Default::Path(path) => format!("{path}()"),
    })
}

/// `Path(f0, f1)` or `Path { a: f0, b: f1 }`.
fn construct(path: &str, fields: &Fields) -> String {
    match fields {
        Fields::Named(fields) => {
            let members: Vec<String> = fields
                .iter()
                .map(|f| format!("{}: f{}", f.member, f.index))
                .collect();
            format!("{path} {{ {} }}", members.join(", "))
        }
        Fields::Tuple(fields) => {
            let members: Vec<String> = fields.iter().map(|f| format!("f{}", f.index)).collect();
            format!("{path}({})", members.join(", "))
        }
        Fields::Unit => path.to_owned(),
    }
}

/// A closure that reads the fields in order from an array.
fn positional(path: &str, fields: &Fields, list: &[Field], what: &str) -> String {
    let plural = if list.len() == 1 { "" } else { "s" };
    let expected = quote(&format!("{what} with {} element{plural}", list.len()));
    let mut code = String::new();
    for field in list {
        let missing = default(field).unwrap_or_else(|| {
            format!(
                "return {ERR}(::alt_json::Error::invalid_length({}, {expected}))",
                field.index
            )
        });
        code += &format!(
            "let f{} = match s.next_with({})? {{ {SOME}(value) => value, {NONE} => {missing} }};",
            field.index,
            reader(field)
        );
    }
    format!("|s| {{ {code} {OK}({}) }}", construct(path, fields))
}

/// A closure that reads the fields by name from an object.
fn keyed(input: &Input, path: &str, fields: &Fields, list: &[Field]) -> String {
    let names: Vec<String> = list.iter().map(|f| quote(&input.field_name(f))).collect();
    let mut slots = String::new();
    let mut lookup = String::new();
    let mut arms = String::new();
    let mut finish = String::new();
    for (field, name) in list.iter().zip(&names) {
        let index = field.index;
        slots += &format!("let mut f{index} = {NONE};");
        lookup += &format!("{name} => {OK}({index}usize),");
        arms += &format!(
            "{index}usize => {{ if f{index}.is_some() {{ \
             return {ERR}(::alt_json::Error::duplicate_field({name})); }} \
             f{index} = {SOME}(m.next_value_with({})?); }}",
            reader(field)
        );
        let missing = match (default(field), &field.attrs.with) {
            (Some(default), _) => default,
            (None, Some(_)) => format!("return {ERR}(::alt_json::Error::missing_field({name}))"),
            (None, None) => format!("::alt_json::FromJson::missing_field({name})?"),
        };
        finish += &format!(
            "let f{index} = match f{index} {{ {SOME}(value) => value, {NONE} => {missing} }};"
        );
    }
    let unknown = if input.attrs.deny_unknown_fields {
        format!(
            "{ERR}(::alt_json::Error::unknown_field(key, &[{}]))",
            names.join(", ")
        )
    } else {
        format!("{OK}(usize::MAX)")
    };
    format!(
        "|m| {{ {slots} \
         while let {SOME}(index) = m.next_key(|key| -> ::alt_json::Result<usize> {{ \
         match key {{ {lookup} _ => {unknown} }} }})? {{ \
         match index {{ {arms} _ => m.ignore_value()?, }} }} \
         {finish} {OK}({}) }}",
        construct(path, fields)
    )
}

/// An expression that reads the data of a struct or variant from `d`.
fn data(input: &Input, path: &str, fields: &Fields, what: &str) -> String {
    let expected = quote(what);
    match fields {
        Fields::Named(list) => format!(
            "d.structure({expected}, {}, {})",
            positional(path, fields, list, what),
            keyed(input, path, fields, list)
        ),
        Fields::Tuple(list) => match list.as_slice() {
            [only] => format!("{}(d).map({path})", reader(only)),
            _ => format!(
                "d.seq({expected}, {})",
                positional(path, fields, list, what)
            ),
        },
        Fields::Unit => format!("d.unit({expected}).map(|()| {path})"),
    }
}

fn struct_body(input: &Input, fields: &Fields) -> (String, Option<String>) {
    let kind = match fields {
        Fields::Named(_) => "struct",
        Fields::Tuple(list) if list.len() == 1 => "newtype struct",
        Fields::Tuple(_) => "tuple struct",
        Fields::Unit => "unit struct",
    };
    let body = data(input, "Self", fields, &format!("{kind} {}", input.name));
    let key_body = match fields {
        Fields::Tuple(list) if list.len() == 1 => {
            Some("::alt_json::FromJson::from_json_key(d).map(Self)".to_owned())
        }
        _ => None,
    };
    (body, key_body)
}

/// A closure that maps a variant name to its position.
fn lookup(input: &Input, variants: &[Variant]) -> String {
    let names: Vec<String> = variants
        .iter()
        .map(|v| quote(&input.variant_name(v)))
        .collect();
    let arms: String = names
        .iter()
        .enumerate()
        .map(|(index, name)| format!("{name} => {OK}({index}usize),"))
        .collect();
    format!(
        "|name: &str| match name {{ {arms} _ => {ERR}(::alt_json::Error::unknown_variant(name, &[{}])) }}",
        names.join(", ")
    )
}

fn enum_body(input: &Input, variants: &[Variant]) -> (String, Option<String>) {
    let lookup = lookup(input, variants);
    let unreachable =
        format!("_ => {ERR}(::alt_json::Error::custom(\"variant index out of range\")),");
    let name = &input.name;
    let mut arms = String::new();
    for (index, variant) in variants.iter().enumerate() {
        let path = format!("Self::{}", variant.name);
        let what = match variant.fields {
            Fields::Named(_) => format!("struct variant {name}::{}", variant.name),
            Fields::Tuple(_) => format!("tuple variant {name}::{}", variant.name),
            Fields::Unit => format!("unit variant {name}::{}", variant.name),
        };
        let arm = match (&input.attrs.tag, &input.attrs.content) {
            (None, _) => external_arm(input, variant, &path, &what),
            (Some(_), None) => match variant.fields {
                Fields::Unit => format!("d.tagged_unit().map(|()| {path})"),
                _ => data(input, &path, &variant.fields, &what),
            },
            (Some(_), Some(content)) => {
                let content = quote(content);
                let absent = match &variant.fields {
                    Fields::Unit => format!("{OK}({path})"),
                    Fields::Tuple(list) if list.len() == 1 && list[0].attrs.with.is_none() => {
                        format!("::alt_json::FromJson::missing_field({content}).map({path})")
                    }
                    _ => format!("{ERR}(::alt_json::Error::missing_field({content}))"),
                };
                let present = match &variant.fields {
                    Fields::Named(list) => format!(
                        "d.struct_map({}, {})",
                        quote(&what),
                        keyed(input, &path, &variant.fields, list)
                    ),
                    Fields::Unit => format!("d.content_unit({}).map(|()| {path})", quote(&what)),
                    Fields::Tuple(_) => data(input, &path, &variant.fields, &what),
                };
                format!("if present {{ {present} }} else {{ {absent} }}")
            }
        };
        arms += &format!("{index}usize => {arm},");
    }
    match (&input.attrs.tag, &input.attrs.content) {
        (None, _) => (
            format!("d.enumeration({lookup}, |v, index| match index {{ {arms} {unreachable} }})"),
            Some("<Self as ::alt_json::FromJson>::from_json(d)".to_owned()),
        ),
        (Some(tag), None) => (
            format!(
                "d.internally_tagged({}, {}, {lookup}, |d, index| match index {{ {arms} {unreachable} }})",
                quote(tag),
                quote(&format!("internally tagged enum {name}"))
            ),
            None,
        ),
        (Some(tag), Some(content)) => (
            format!(
                "d.adjacently_tagged(({}, {}), {}, {}, {lookup}, \
                 |d, index, present| match index {{ {arms} {unreachable} }})",
                quote(tag),
                quote(content),
                input.attrs.deny_unknown_fields,
                quote(&format!("adjacently tagged enum {name}"))
            ),
            None,
        ),
    }
}

/// Reads a variant in the default form, where `v` holds its data.
fn external_arm(input: &Input, variant: &Variant, path: &str, what: &str) -> String {
    let expected = quote(what);
    match &variant.fields {
        Fields::Named(list) => format!(
            "v.structure({expected}, {}, {})",
            positional(path, &variant.fields, list, what),
            keyed(input, path, &variant.fields, list)
        ),
        Fields::Tuple(list) => match list.as_slice() {
            [only] => format!("v.newtype_with({}).map({path})", reader(only)),
            _ => format!(
                "v.tuple({expected}, {})",
                positional(path, &variant.fields, list, what)
            ),
        },
        Fields::Unit => format!("v.unit().map(|()| {path})"),
    }
}
