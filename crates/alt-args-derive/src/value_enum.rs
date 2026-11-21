//! The implementation of `Value` for an enum of unit variants.

use crate::Error;
use crate::describe::quote;
use crate::field::kebab;
use crate::parse::{Data, Input, Shape as VariantShape};

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
           const POSSIBLE: &'static str = {names}; \
           const CHECK: ::core::option::Option<::alt_args::Check> = ::core::option::Option::None; \
           fn parse(text: &str) -> ::core::result::Result<Self, ::std::string::String> {{ \
             match text {{ {arms} _ => ::core::result::Result::Err(::std::string::String::new()) }} }} }}",
        ty = input.name,
        names = quote(&names.join("\n")),
    ))
}
