//! The help of every command of the command line, at every level of its
//! subcommands, is the recorded one: a change to what a command takes or
//! says of itself shows as a change to `fixtures/args/help.txt`.
//! `ALT_HELP_RECORD=1` rewrites it.

use alt_args::{Command, ErrorKind, Parser};
use alt_cli::cli::Cli;

/// Adds the help of the command at `path`, then of every command below it.
fn record(command: &Command, path: &mut Vec<&'static str>, helps: &mut Vec<String>) {
    let words: Vec<&str> = ["alt"].into_iter().chain(path.iter().copied()).collect();
    let name = words.join(" ");
    let words = words.into_iter().chain(["--help"]).map(String::from);
    let Err(help) = Cli::try_parse_from(words) else {
        panic!("{name}: --help was taken for an argument");
    };
    assert_eq!(help.kind, ErrorKind::Help, "{name}: {help}");
    helps.push(format!("### {name}\n{help}"));
    for below in command.subcommands() {
        path.push(below.name);
        record(&below, path, helps);
        path.pop();
    }
}

#[test]
fn every_command_gives_its_recorded_help() {
    let mut helps = Vec::new();
    record(&Cli::command(), &mut Vec::new(), &mut helps);
    if std::env::var_os("ALT_HELP_RECORD").is_some() {
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/args/help.txt");
        std::fs::write(fixture, helps.concat()).unwrap();
        return;
    }
    let recorded = include_str!("../fixtures/args/help.txt");
    let recorded: Vec<&str> = recorded.split_inclusive("\n").collect();
    let mut at = 0;
    for help in &helps {
        let lines = help.split_inclusive('\n').count();
        let was: String = recorded.iter().skip(at).take(lines).copied().collect();
        assert_eq!(*help, was, "line {} of the fixture", at + 1);
        at += lines;
    }
    assert_eq!(at, recorded.len(), "the fixture records more");
    // the top level, its commands, and at least one level below them
    assert!(helps.len() > 100, "{}", helps.len());
    let below = "### alt flow feature start\n";
    assert!(helps.iter().any(|help| help.starts_with(below)));
}
