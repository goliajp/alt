//! The `--help` text.

use crate::scope::{Scope, Slot};
use crate::spec::Preset;
use crate::usage;

/// the width the layout rules assume; lines are never wrapped
const WIDTH: usize = 100;
const NEXT_LINE_INDENT: usize = 10;

struct Row {
    /// the indented name column
    name: String,
    /// what the name contributes to the column width, if anything
    measured: Option<usize>,
    help: String,
    /// the length of the description as the layout rules count it
    weight: usize,
}

fn section(title: &str, rows: &[Row]) -> String {
    let longest = rows
        .iter()
        .filter_map(|row| row.measured)
        .max()
        .unwrap_or(0);
    let column = longest + 4;
    // a wide name column with a long description moves every description
    // of the section to its own line
    let below = column * 5 > WIDTH * 2
        && column <= WIDTH
        && rows.iter().any(|row| row.weight > WIDTH - column);
    let mut text = format!("{title}:\n");
    for row in rows {
        if below {
            text += &format!("{}\n{:NEXT_LINE_INDENT$}{}\n", row.name, "", row.help);
        } else {
            text += &format!("{:column$}{}\n", row.name, row.help);
        }
    }
    text
}

fn preset(slot: &Slot) -> Option<String> {
    match slot.arg.preset {
        Preset::None => None,
        Preset::Text(text) => Some(text.to_owned()),
        Preset::Computed(compute) => Some(compute()),
    }
}

/// The help of an argument followed by its bracketed notes.
fn description(slot: &Slot) -> (String, usize) {
    let arg = slot.arg;
    let mut parts = Vec::new();
    if let Some(value) = preset(slot) {
        parts.push(format!("[default: {value}]"));
    }
    if !arg.alias.is_empty() {
        parts.push(format!("[aliases: --{}]", arg.alias));
    }
    if !arg.possible.is_empty() {
        parts.push(format!("[possible values: {}]", arg.possible.join(", ")));
    }
    let notes = parts.join(" ");
    let weight = arg.help.len() + notes.len();
    match (arg.help.is_empty(), notes.is_empty()) {
        (_, true) => (arg.help.to_owned(), weight),
        (true, false) => (notes, weight),
        (false, false) => (format!("{} {notes}", arg.help), weight),
    }
}

fn option(short: Option<char>, long: &str, value: &str, (help, weight): (String, usize)) -> Row {
    let name = match (short, long.is_empty()) {
        (Some(short), true) => format!("  -{short}{value}"),
        (Some(short), false) => format!("  -{short}, --{long}{value}"),
        (None, _) => format!("      --{long}{value}"),
    };
    let spelled = if long.is_empty() { 2 } else { 2 + long.len() };
    Row {
        name,
        measured: (!value.is_empty() || !long.is_empty()).then_some(spelled + value.len() + 4),
        help,
        weight,
    }
}

/// short options before long ones, lower case before upper case
fn sort_key(slot: &Slot) -> (usize, String) {
    let key = match slot.arg.short {
        Some(short) => format!(
            "{}{}",
            short.to_ascii_lowercase(),
            if short.is_ascii_lowercase() { '0' } else { '1' }
        ),
        None => slot.arg.long.to_owned(),
    };
    (slot.order, key)
}

fn options(scope: &Scope) -> Vec<Row> {
    let mut slots: Vec<&Slot> = scope
        .slots
        .iter()
        .filter(|slot| !slot.arg.is_positional())
        .collect();
    slots.sort_by_key(|slot| sort_key(slot));
    slots
        .into_iter()
        .map(|slot| {
            let value = match slot.arg.takes_value() {
                true => format!(" <{}>", slot.arg.value_name),
                false => String::new(),
            };
            option(slot.arg.short, slot.arg.long, &value, description(slot))
        })
        .collect()
}

pub(crate) const HELP_ABOUT: &str = "Print this message or the help of the given subcommand(s)";

pub(crate) fn render(scope: &Scope) -> String {
    let mut text = String::new();
    if !scope.command.about.is_empty() {
        text += &format!("{}\n\n", scope.command.about);
    }
    text += &format!("{}\n", usage::full(scope));
    let mut commands: Vec<Row> = Vec::new();
    for command in scope.command.subcommands {
        commands.push(Row {
            name: format!("  {}", command.name),
            measured: Some(command.name.len()),
            help: command.about.to_owned(),
            weight: command.about.len(),
        });
    }
    if scope.command.help_subcommand {
        commands.push(Row {
            name: "  help".to_owned(),
            measured: Some(4),
            help: HELP_ABOUT.to_owned(),
            weight: HELP_ABOUT.len(),
        });
    }
    if !commands.is_empty() {
        text += &format!("\n{}", section("Commands", &commands));
    }
    let arguments: Vec<Row> = scope
        .positionals()
        .map(|(_, slot)| {
            let name = slot.arg.display(None);
            let (help, weight) = description(slot);
            Row {
                measured: Some(name.len()),
                name: format!("  {name}"),
                help,
                weight,
            }
        })
        .collect();
    if !arguments.is_empty() {
        text += &format!("\n{}", section("Arguments", &arguments));
    }
    text += &format!("\n{}", section("Options", &options(scope)));
    text
}

/// The help of the `help` subcommand itself.
pub(crate) fn of_help(scope: &Scope) -> String {
    format!(
        "{HELP_ABOUT}\n\nUsage: {} help [COMMAND]...\n\nArguments:\n  [COMMAND]...  Print help for the subcommand(s)\n",
        scope.path
    )
}
