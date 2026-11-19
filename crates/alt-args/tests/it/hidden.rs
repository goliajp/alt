//! A hidden subcommand is parsed like any other and never shown.

use alt_args::{Arg, Command, ErrorKind, Item, Kind, parse};

static TOOL: Command = Command {
    name: "tool",
    about: "",
    version: "",
    help_subcommand: true,
    hidden: false,
    items: &[
        Item::Arg(Arg {
            id: "verbose",
            short: Some('v'),
            ..Arg::NEW
        }),
        Item::Subcommands,
    ],
    subcommands: &[
        Command {
            name: "status",
            about: "Show the state",
            version: "",
            help_subcommand: false,
            hidden: false,
            items: &[],
            subcommands: &[],
        },
        Command {
            name: "statuz",
            about: "Answer the shell",
            version: "",
            help_subcommand: false,
            hidden: true,
            items: &[Item::Arg(Arg {
                id: "word",
                long: "word",
                value_name: "WORD",
                kind: Kind::Single,
                ..Arg::NEW
            })],
            subcommands: &[],
        },
    ],
};

fn refused(line: &str) -> String {
    let error = parse(&TOOL, line.split(' ').map(String::from))
        .err()
        .unwrap();
    error.to_string()
}

#[test]
fn a_hidden_subcommand_runs() {
    let words = "tool statuz --word here".split(' ').map(String::from);
    let matches = parse(&TOOL, words).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(matches.sub().name(), "statuz");
    assert_eq!(matches.sub().one("word"), Some("here"));
    let words = "tool statuz --help".split(' ').map(String::from);
    let help = parse(&TOOL, words).err().unwrap();
    assert_eq!(help.kind, ErrorKind::Help);
    assert!(help.to_string().contains("Usage: tool statuz [OPTIONS]"));
}

#[test]
fn help_and_the_subcommand_list_leave_it_out() {
    let help = refused("tool --help");
    assert!(help.contains("  status  Show the state\n"), "{help}");
    assert!(!help.contains("statuz"), "{help}");
    let missing = refused("tool -v");
    assert!(missing.contains("[subcommands: status, help]"), "{missing}");
}

#[test]
fn suggestions_leave_it_out() {
    let unknown = refused("tool statu");
    assert!(
        unknown.contains("tip: a similar subcommand exists: 'status'\n"),
        "{unknown}"
    );
    let option = refused("tool --wrd statuz");
    assert!(option.contains("unexpected argument '--wrd'"), "{option}");
    assert!(!option.contains("statuz --word"), "{option}");
}
