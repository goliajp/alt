//! A positional that takes the words after `--` and no other.

use alt_args::{ErrorKind, Item, Items, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "t")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Reset {
        revs: Vec<String>,
        #[arg(last = true)]
        paths: Vec<String>,
        #[arg(long)]
        hard: bool,
    },
    Show {
        rev: Option<String>,
        #[arg(last = true)]
        paths: Vec<String>,
        #[arg(dashed = true)]
        dashed: bool,
    },
}

fn reset(line: &str) -> (Vec<String>, Vec<String>, bool) {
    let cli = Cli::try_parse_from(line.split(' ').map(String::from));
    match cli.unwrap_or_else(|e| panic!("{e}")).command {
        Command::Reset { revs, paths, hard } => (revs, paths, hard),
        Command::Show { .. } => panic!("{line}"),
    }
}

#[test]
fn the_words_after_two_dashes_and_no_others_go_to_it() {
    let read = |line: &str| {
        let (revs, paths, hard) = reset(line);
        (revs.join(" "), paths.join(" "), hard)
    };
    assert_eq!(read("t reset a b"), ("a b".into(), "".into(), false));
    assert_eq!(
        read("t reset a -- b -c"),
        ("a".into(), "b -c".into(), false)
    );
    assert_eq!(read("t reset -- b"), ("".into(), "b".into(), false));
    assert_eq!(read("t reset --"), ("".into(), "".into(), false));
    assert_eq!(
        read("t reset a --hard b -- --hard"),
        ("a b".into(), "--hard".into(), true)
    );
    assert_eq!(
        read("t reset a -- b -- c"),
        ("a".into(), "b -- c".into(), false)
    );
}

#[test]
fn a_word_too_many_before_the_dashes_is_not_taken_for_it() {
    let line = "t show a b".split(' ').map(String::from);
    let Err(refused) = Cli::try_parse_from(line) else {
        panic!("taken")
    };
    assert_eq!(refused.kind, ErrorKind::Usage);
    let line = "t show a -- b c".split(' ').map(String::from);
    let Ok(cli) = Cli::try_parse_from(line) else {
        panic!("refused")
    };
    let Command::Show { rev, paths, .. } = cli.command else {
        panic!()
    };
    assert_eq!((rev.as_deref(), paths.join(" ")), (Some("a"), "b c".into()));
}

#[test]
fn it_is_described_and_shown_after_the_dashes() {
    let command = Cli::command().subcommand("reset").unwrap();
    fn args(items: Items, found: &mut Vec<(&'static str, bool)>) {
        for item in items {
            match item {
                Item::Arg(arg) => found.push((arg.id, arg.last)),
                Item::Group(_, items) => args(items, found),
                Item::Subcommands => {}
            }
        }
    }
    let mut described = Vec::new();
    args(command.items(), &mut described);
    assert_eq!(
        described,
        [("revs", false), ("paths", true), ("hard", false)]
    );
    let line = "t reset --help".split(' ').map(String::from);
    let Err(help) = Cli::try_parse_from(line) else {
        panic!("no help")
    };
    let usage = "Usage: t reset [OPTIONS] [REVS]... [-- <PATHS>...]\n";
    assert!(help.to_string().contains(usage), "{help}");
}

#[test]
fn whether_the_dashes_were_written_is_told() {
    let show = |line: &str| {
        let words = line.split(' ').map(String::from);
        let matches = alt_args::parse(Cli::command(), words).unwrap_or_else(|e| panic!("{e}"));
        let alone = matches.sub().dashes_alone();
        let Command::Show { paths, dashed, .. } = Cli::from_matches(&matches).command else {
            panic!("{line}")
        };
        (dashed, alone, paths.join(" "))
    };
    assert_eq!(show("t show a"), (false, false, "".into()));
    assert_eq!(show("t show a --"), (true, true, "".into()));
    assert_eq!(show("t show --"), (true, true, "".into()));
    assert_eq!(show("t show a -- b"), (true, false, "b".into()));
    assert_eq!(show("t show a -- --"), (true, false, "--".into()));
    // the field is no argument: it is not described, and no word sets it
    let command = Cli::command().subcommand("show").unwrap();
    let ids: Vec<_> = command
        .items()
        .flat_map(|item| match item {
            Item::Group(_, items) => items.collect::<Vec<_>>(),
            item => vec![item],
        })
        .filter_map(|item| match item {
            Item::Arg(arg) => Some(arg.id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["rev", "paths"]);
    let line = "t show --dashed".split(' ').map(String::from);
    assert!(Cli::try_parse_from(line).is_err());
}
