//! What is offered at the cursor of an unfinished command line.
#![allow(dead_code)]

mod options;
mod values;

use alt_args::complete::{Candidate, Completion, complete};
use alt_args::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tool", version)]
struct Tool {
    #[command(subcommand)]
    command: Op,
    /// Work elsewhere
    #[arg(long, global = true, value_name = "NAME", hint = "workspace")]
    workspace: Option<String>,
    /// Say more
    #[arg(short = 'v', long, action = count)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Op {
    /// Record changes
    Commit {
        /// What changed
        #[arg(short = 'm', long, hint = none)]
        message: Option<String>,
        #[arg(short = 'a', long, visible_alias = "everything")]
        all: bool,
        #[arg(long, conflicts_with = "all")]
        amend: bool,
        #[arg(short = 'n', long)]
        no_verify: bool,
        #[arg(long = "decision")]
        decisions: Vec<String>,
        #[arg(short = 'u', long, default_missing_value = "all", value_parser = ["no", "all"])]
        untracked: Option<String>,
        #[arg(short = 't', group = "show")]
        show_type: bool,
        #[arg(short = 's', group = "show")]
        show_size: bool,
        #[arg(hint = path)]
        paths: Vec<String>,
    },
    /// Go to a branch
    Switch {
        #[arg(hint = "branch")]
        branch: String,
        #[arg(hint = dir)]
        dir: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Run a command
    Run {
        #[arg(long)]
        quiet: bool,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Look for a pattern
    Grep {
        #[arg(allow_hyphen_values = true)]
        pattern: String,
        #[arg(long, short = 'c')]
        count: bool,
        #[arg(short = 'e')]
        engine: Option<String>,
    },
    /// Manage remotes
    Remote {
        #[command(subcommand)]
        op: RemoteOp,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Shelve changes
    Stash {
        #[command(subcommand)]
        op: StashOp,
        name: Option<String>,
    },
    #[command(hide)]
    Answer { word: Option<String> },
}

#[derive(Subcommand)]
enum RemoteOp {
    /// Add one
    Add { name: String },
    /// List them
    List,
}

#[derive(Subcommand)]
enum StashOp {
    Push,
    Pop,
}

fn names(candidates: &[Candidate]) -> String {
    let names: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    names.join(" ")
}

fn show(found: Vec<Completion>) -> Vec<String> {
    let show = |completion: &Completion| match completion {
        Completion::Subcommands(candidates) => format!("commands: {}", names(candidates)),
        Completion::Options(candidates) => format!("options: {}", names(candidates)),
        Completion::Value {
            arg,
            prefix,
            attached,
        } => match attached {
            Some(attached) => format!("value {} [{prefix}] behind [{attached}]", arg.id),
            None => format!("value {} [{prefix}]", arg.id),
        },
    };
    found.iter().map(show).collect()
}

/// What is offered for the last word of `line`, which is empty when the
/// line ends in a space.
fn at(line: &str) -> Vec<String> {
    let words: Vec<String> = line.split(' ').map(String::from).collect();
    show(complete(Tool::COMMAND, &words, words.len() - 1))
}

const COMMANDS: &str = "commit switch run grep remote stash";

#[test]
fn subcommands_are_offered_where_one_is_expected() {
    let listed = format!("commands: {COMMANDS} help");
    assert_eq!(at("tool "), [listed.as_str()]);
    assert_eq!(at("tool sw"), [listed.as_str()]);
    assert_eq!(at("tool remote "), ["commands: add list"]);
    let found = complete(Tool::COMMAND, &["tool".to_owned()], 1);
    let [Completion::Subcommands(candidates)] = found.as_slice() else {
        panic!()
    };
    assert_eq!(candidates[0].help, "Record changes");
    assert!(candidates.last().unwrap().help.starts_with("Print this"));
}

#[test]
fn a_hidden_subcommand_is_not_offered_but_completes() {
    assert!(!at("tool a")[0].contains("answer"));
    assert_eq!(at("tool answer "), ["value word []"]);
}

#[test]
fn after_help_the_subcommands_of_the_named_command_are_offered() {
    let listed = format!("commands: {COMMANDS}");
    assert_eq!(at("tool help "), [listed.as_str()]);
    assert_eq!(at("tool --workspace w help re"), [listed.as_str()]);
    assert_eq!(at("tool help remote "), ["commands: add list"]);
    assert!(at("tool help remote add ").is_empty());
    assert!(at("tool help nonesuch ").is_empty());
    // below the top `help` is no subcommand: a stray word, passed over
    assert_eq!(at("tool remote help "), ["commands: add list"]);
}

#[test]
fn a_positional_and_subcommands_are_offered_together() {
    assert_eq!(at("tool stash "), ["value name []", "commands: push pop"]);
    assert_eq!(at("tool stash p"), ["value name [p]", "commands: push pop"]);
    assert_eq!(at("tool stash mine "), ["commands: push pop"]);
    assert_eq!(at("tool stash mine pop "), Vec::<String>::new());
}

#[test]
fn the_cursor_may_be_anywhere() {
    let words: Vec<String> = "tool switch main".split(' ').map(String::from).collect();
    assert!(complete(Tool::COMMAND, &words, 0).is_empty());
    assert_eq!(
        show(complete(Tool::COMMAND, &words, 1)),
        [format!("commands: {COMMANDS} help")]
    );
    assert_eq!(
        show(complete(Tool::COMMAND, &words, 2)),
        ["value branch [main]"]
    );
    assert_eq!(show(complete(Tool::COMMAND, &words, 3)), ["value dir []"]);
    assert_eq!(show(complete(Tool::COMMAND, &words, 9)), ["value dir []"]);
    assert!(complete(Tool::COMMAND, &[], 0).is_empty());
    assert_eq!(
        show(complete(Tool::COMMAND, &[], 1)),
        [format!("commands: {COMMANDS} help")]
    );
}

#[test]
fn a_line_already_wrong_is_read_as_far_as_it_goes() {
    assert_eq!(at("tool commit --nonesuch -m "), ["value message []"]);
    assert_eq!(at("tool commit -m -a "), ["value paths []"]);
    assert_eq!(at("tool commit -a -a --all=x -Z "), ["value paths []"]);
    assert_eq!(at("tool commit --untracked=some "), ["value paths []"]);
    assert_eq!(at("tool switch a b c d "), Vec::<String>::new());
    assert_eq!(at("tool --help commit --version "), ["value paths []"]);
    assert_eq!(at("tool nonesuch "), [format!("commands: {COMMANDS} help")]);
    assert_eq!(
        at("tool nonesuch commit -m"),
        ["value message [] behind [-m]"]
    );
}
