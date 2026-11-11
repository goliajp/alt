//! Generated `ToJson` implementations.

use crate::parse::{Data, Field, Fields, Input, Variant, quote};

const KEY_ERROR: &str = "::core::result::Result::Err(::alt_json::Error::key_must_be_a_string())";

pub(crate) fn generate(input: &Input) -> String {
    let (body, key_body) = match (&input.attrs.into, &input.data) {
        (Some(target), _) => {
            let converted = format!(
                "&<{target} as ::core::convert::From<Self>>::from(::core::clone::Clone::clone(self))"
            );
            (
                format!("::alt_json::ToJson::to_json({converted}, w)"),
                Some(format!("::alt_json::ToJson::to_json_key({converted}, w)")),
            )
        }
        (None, Data::Struct(fields)) => struct_body(input, fields),
        (None, Data::Enum(variants)) => enum_body(input, variants),
    };
    let key_method = key_body.map_or(String::new(), |key_body| {
        format!("fn to_json_key(&self, w: &mut ::alt_json::Writer) -> ::alt_json::Result<()> {{ {key_body} }}")
    });
    format!(
        "{} {{ fn to_json(&self, w: &mut ::alt_json::Writer) -> ::alt_json::Result<()> {{ {body} }} {key_method} }}",
        input.impl_header("ToJson")
    )
}

/// Writes one value; `place` is an expression that is a reference to it.
fn write(field: &Field, place: &str) -> String {
    match &field.attrs.with {
        Some(module) => format!("{module}::to_json({place}, w)?;"),
        None => format!("::alt_json::ToJson::to_json({place}, w)?;"),
    }
}

/// The members of an object, one per field.
fn members(input: &Input, fields: &[Field], place: impl Fn(&Field) -> String) -> String {
    let mut code = String::new();
    for field in fields {
        let place = place(field);
        let member = format!(
            "w.key({}); {}",
            quote(&input.field_name(field)),
            write(field, &place)
        );
        code += &match &field.attrs.skip_serializing_if {
            Some(skip) => format!("if !{skip}({place}) {{ {member} }}"),
            None => member,
        };
    }
    code
}

/// The elements of an array, one per field.
fn elements(fields: &[Field], place: impl Fn(&Field) -> String) -> String {
    let mut code = String::from("w.begin_array()?;");
    for field in fields {
        code += &format!("w.element(); {}", write(field, &place(field)));
    }
    code + "w.end_array();"
}

fn struct_body(input: &Input, fields: &Fields) -> (String, Option<String>) {
    let place = |field: &Field| format!("&self.{}", field.member);
    match fields {
        Fields::Named(fields) => (
            format!(
                "w.begin_object()?; {} w.end_object(); ::core::result::Result::Ok(())",
                members(input, fields, place)
            ),
            None,
        ),
        Fields::Tuple(fields) => match fields.as_slice() {
            [only] => (
                format!("{} ::core::result::Result::Ok(())", write(only, "&self.0")),
                Some("::alt_json::ToJson::to_json_key(&self.0, w)".to_owned()),
            ),
            _ => (
                format!("{} ::core::result::Result::Ok(())", elements(fields, place)),
                None,
            ),
        },
        Fields::Unit => ("w.unit()".to_owned(), None),
    }
}

/// The pattern that binds every field of a variant to `f<position>`.
fn pattern(variant: &Variant) -> String {
    match &variant.fields {
        Fields::Named(fields) => {
            let bindings: Vec<String> = fields
                .iter()
                .map(|f| format!("{}: {}", f.member, binding(f)))
                .collect();
            format!("Self::{} {{ {} }}", variant.name, bindings.join(", "))
        }
        Fields::Tuple(fields) => {
            let bindings: Vec<String> = fields.iter().map(binding).collect();
            format!("Self::{}({})", variant.name, bindings.join(", "))
        }
        Fields::Unit => format!("Self::{}", variant.name),
    }
}

fn binding(field: &Field) -> String {
    format!("f{}", field.index)
}

/// The data of a variant as one value, or nothing for a unit variant.
fn variant_data(input: &Input, variant: &Variant) -> Option<String> {
    match &variant.fields {
        Fields::Named(fields) => Some(format!(
            "w.begin_object()?; {} w.end_object();",
            members(input, fields, binding)
        )),
        Fields::Tuple(fields) => Some(match fields.as_slice() {
            [only] => write(only, &binding(only)),
            _ => elements(fields, binding),
        }),
        Fields::Unit => None,
    }
}

fn enum_body(input: &Input, variants: &[Variant]) -> (String, Option<String>) {
    let mut arms = String::new();
    let mut key_arms = String::new();
    for variant in variants {
        let name = quote(&input.variant_name(variant));
        let data = variant_data(input, variant);
        let body = match (&input.attrs.tag, &input.attrs.content) {
            (None, _) => match data {
                Some(data) => format!("w.begin_object()?; w.key({name}); {data} w.end_object();"),
                None => format!("w.unit_variant({name})?;"),
            },
            (Some(tag), Some(content)) => {
                let data = data.map_or(String::new(), |data| {
                    format!("w.key({}); {data}", quote(content))
                });
                format!(
                    "w.begin_object()?; w.key({}); w.string({name})?; {data} w.end_object();",
                    quote(tag)
                )
            }
            (Some(tag), None) => {
                let tag = quote(tag);
                match &variant.fields {
                    Fields::Named(fields) => format!(
                        "w.begin_object()?; w.key({tag}); w.string({name})?; {} w.end_object();",
                        members(input, fields, binding)
                    ),
                    Fields::Tuple(_) => {
                        format!("w.tagged({tag}, {name}); {}", data.unwrap_or_default())
                    }
                    Fields::Unit => {
                        format!(
                            "w.begin_object()?; w.key({tag}); w.string({name})?; w.end_object();"
                        )
                    }
                }
            }
        };
        arms += &format!(
            "{} => {{ {body} ::core::result::Result::Ok(()) }}",
            pattern(variant)
        );
        key_arms += &match variant.fields {
            Fields::Unit => format!(
                "Self::{} => {{ w.key({name}); ::core::result::Result::Ok(()) }},",
                variant.name
            ),
            _ => format!("Self::{} {{ .. }} => {KEY_ERROR},", variant.name),
        };
    }
    if variants.is_empty() {
        // no value exists to be written
        return ("match *self {}".to_owned(), None);
    }
    let key_body = match input.attrs.tag {
        None => Some(format!("match self {{ {key_arms} }}")),
        Some(_) => None,
    };
    (format!("match self {{ {arms} }}"), key_body)
}
