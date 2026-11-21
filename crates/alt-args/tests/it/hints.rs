//! `#[arg(hint = ...)]` and `#[command(hide)]` reach the description.
#![allow(dead_code)]

use alt_args::{Hint, Item, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tool")]
struct Tool {
    #[command(subcommand)]
    command: Op,
}

#[derive(Subcommand)]
enum Op {
    Open {
        #[arg(hint = path)]
        file: String,
        #[arg(long, hint = dir)]
        into: Option<String>,
        #[arg(long, hint = "branch")]
        branch: Option<String>,
        #[arg(long, hint = none)]
        message: Option<String>,
        #[arg(long)]
        plain: Option<String>,
    },
    #[command(hide)]
    Answer,
    #[command(hide = false, name = "show")]
    Shown,
}

#[test]
fn hints_are_recorded_per_argument() {
    let open = Tool::command().subcommand("open").unwrap();
    let Some(Item::Group(_, items)) = open.items().next() else {
        panic!()
    };
    let hints = items.map(|item| match item {
        Item::Arg(arg) => (arg.id, arg.hint()),
        _ => panic!(),
    });
    assert_eq!(
        hints.collect::<Vec<_>>(),
        [
            ("file", Hint::Path),
            ("into", Hint::Dir),
            ("branch", Hint::Other("branch")),
            ("message", Hint::None),
            ("plain", Hint::Unset),
        ]
    );
}

#[test]
fn hide_marks_the_subcommand() {
    let commands = Tool::command().subcommands();
    assert_eq!(
        commands.map(|c| (c.name, c.hidden)).collect::<Vec<_>>(),
        [("open", false), ("answer", true), ("show", false)]
    );
    let words = ["tool", "answer"].map(String::from);
    assert!(matches!(
        Tool::try_parse_from(words).map(|tool| tool.command),
        Ok(Op::Answer)
    ));
    let help = Tool::try_parse_from(["tool", "--help"].map(String::from));
    let help = help.err().unwrap().to_string();
    assert!(
        help.contains("\n  show") && !help.contains("answer"),
        "{help}"
    );
}
