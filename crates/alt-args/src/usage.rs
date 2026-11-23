//! The `Usage:` line.

use crate::arg::Arg;
use crate::scope::Scope;
use crate::settings::Kind;

fn finish(scope: &Scope, mut line: String) -> String {
    if scope.has_subcommands() {
        line += " <COMMAND>";
    }
    line
}

/// The usage shown in help: optional arguments folded into `[OPTIONS]`.
pub(crate) fn full(scope: &Scope) -> String {
    let mut line = format!("Usage: {}", scope.path);
    let options = scope.slots.iter().filter(|slot| !slot.arg.is_positional());
    if options
        .clone()
        .any(|slot| !slot.arg.required && !slot.arg.is_builtin())
    {
        line += " [OPTIONS]";
    }
    for slot in options.filter(|slot| slot.arg.required) {
        line += &format!(" {}", slot.arg.display(None));
    }
    for (_, slot) in scope.positionals() {
        line += &format!(" {}", slot.arg.display(None));
    }
    finish(scope, line)
}

/// The ids of the arguments that are always required.
pub(crate) fn required(scope: &Scope) -> Vec<&'static str> {
    let required = scope.slots.iter().filter(|slot| slot.arg.required);
    required.map(|slot| slot.arg.id).collect()
}

/// The usage shown with an error: `needed` and `used` spelled out, then
/// the remaining positionals as optional.
pub(crate) fn narrowed(scope: &Scope, needed: &[&str], used: &[&str]) -> String {
    if used.is_empty() {
        return full(scope);
    }
    let named = |id: &str| needed.iter().chain(used).any(|other| *other == id);
    let mut options: Vec<String> = Vec::new();
    for id in needed.iter().chain(used) {
        let Some(slot) = scope.find(id).filter(|slot| !slot.arg.is_positional()) else {
            continue;
        };
        let text = slot.arg.display(None);
        if !options.contains(&text) {
            options.push(text);
        }
    }
    let mut line = format!("Usage: {}", scope.path);
    for option in options {
        line += &format!(" {option}");
    }
    for (_, slot) in scope.positionals() {
        line += &format!(" {}", slot.arg.display(Some(named(slot.arg.id))));
    }
    finish(scope, line)
}

impl Arg {
    /// `--long <NAME>`, `-s`, `<NAME>`, `[NAME]...` or `[-- <NAME>...]`; `required` picks the
    /// brackets of a positional and defaults to the argument's own setting.
    pub(crate) fn display(&self, required: Option<bool>) -> String {
        let mut text = if !self.long.is_empty() {
            format!("--{}", self.long)
        } else if let Some(short) = self.short {
            format!("-{short}")
        } else {
            String::new()
        };
        if !self.takes_value() {
            return text;
        }
        if !self.missing().is_empty() {
            return text + &format!("[=<{}>]", self.value_name());
        }
        if !self.is_positional() {
            text.push(' ');
        }
        if self.last {
            return format!("[-- <{}>...]", self.value_name());
        }
        if self.is_positional() && !required.unwrap_or(self.required) {
            text += &format!("[{}]", self.value_name());
        } else {
            text += &format!("<{}>", self.value_name());
        }
        if self.is_positional() && self.kind == Kind::Multiple {
            text += "...";
        }
        text
    }
}
