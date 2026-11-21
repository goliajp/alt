//! Every argument that takes a value says what kind of thing the value
//! is, so that completion knows what to offer for it.

use alt_args::{Command, Hint, Item, Items, Kind, Parser};

fn arguments(items: Items, path: &str, seen: &mut Vec<(String, Hint)>) {
    for item in items {
        match item {
            Item::Arg(arg) if matches!(arg.kind, Kind::Single | Kind::Multiple) => {
                seen.push((format!("{path} <{}>", arg.id), arg.hint()));
            }
            Item::Group(_, items) => arguments(items, path, seen),
            _ => {}
        }
    }
}

fn levels(command: Command, path: &str, seen: &mut Vec<(String, Hint)>) {
    arguments(command.items(), path, seen);
    for subcommand in command.subcommands() {
        levels(subcommand, &format!("{path} {}", subcommand.name), seen);
    }
}

#[test]
fn every_value_has_a_declared_kind() {
    let mut seen = Vec::new();
    levels(alt_cli::cli::Cli::command(), "alt", &mut seen);
    let unmarked: Vec<&str> = seen
        .iter()
        .filter(|(_, hint)| *hint == Hint::Unset)
        .map(|(path, _)| path.as_str())
        .collect();
    assert!(unmarked.is_empty(), "no hint on:\n{}", unmarked.join("\n"));
    let hint = |name: &str| {
        seen.iter()
            .find(|(path, _)| path == name)
            .map(|(_, hint)| *hint)
    };
    // the walk reached global options, spliced-in arguments and nested subcommands
    assert_eq!(hint("alt <workspace>"), Some(Hint::Other("workspace")));
    assert_eq!(hint("alt add <paths>"), Some(Hint::Other("path-add")));
    assert_eq!(
        hint("alt flow feature finish <name>"),
        Some(Hint::Other("flow-topic"))
    );
}
