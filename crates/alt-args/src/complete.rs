//! What may stand at the cursor of an unfinished command line.
//!
//! The words before the cursor are read by the same code that parses a
//! finished line, so an answer never disagrees with what [`parse`] will
//! make of the result. Nothing is filtered by what is already typed at
//! the cursor: matching it is left to the shell.
//!
//! [`parse`]: crate::parse

use crate::level::{Level, State, Step};
use crate::scope::{Letter, Scope, split_long};
use crate::spec::{Arg, Command, Kind};
use crate::{help, validate};

/// One thing to offer, with its line of help.
pub struct Candidate {
    /// the whole word as it would stand on the command line
    pub value: String,
    pub help: &'static str,
}

/// One kind of thing the word at the cursor may be.
pub enum Completion {
    Subcommands(Vec<Candidate>),
    /// the options that may still be given
    Options(Vec<Candidate>),
    /// a value of `arg`, whose candidates the caller knows from its
    /// `possible` and `hint`
    Value {
        arg: &'static Arg,
        /// the part of the word that belongs to the value
        prefix: String,
        /// what stands before the value in the same word (`--name=`,
        /// `-n`); a candidate goes back on the line behind it
        attached: Option<String>,
    },
}

/// What may stand at `words[current]`; `words[0]` is the program, and
/// `current` may be one past the end for a word not yet begun. A line
/// that is already wrong before the cursor is read as far as it makes
/// sense.
pub fn complete(command: &'static Command, words: &[String], current: usize) -> Vec<Completion> {
    if current == 0 {
        return Vec::new();
    }
    let before = words.get(1..current.min(words.len())).unwrap_or(&[]);
    let cursor = words.get(current).map_or("", String::as_str);
    level(&Scope::root(command, String::new()), before, cursor)
}

fn level(scope: &Scope, before: &[String], cursor: &str) -> Vec<Completion> {
    let mut level = Level::new(scope);
    for (at, word) in before.iter().enumerate() {
        let rest = &before[at + 1..];
        match level.word(word, &[]) {
            Ok(Step::Taken) | Err(_) => {}
            Ok(Step::Help) => return help_names(scope, rest),
            Ok(Step::Subcommand(command)) => {
                return self::level(&scope.child(command), rest, cursor);
            }
        }
    }
    at_cursor(&level, cursor)
}

/// After `help`: the subcommands of the command the names lead to.
fn help_names(scope: &Scope, names: &[String]) -> Vec<Completion> {
    let Some((name, rest)) = names.split_first() else {
        return subcommands(scope, false).into_iter().collect();
    };
    match scope.command.subcommands.iter().find(|c| c.name == name) {
        Some(command) => help_names(&scope.child(command), rest),
        None => Vec::new(),
    }
}

fn subcommands(scope: &Scope, with_help: bool) -> Option<Completion> {
    let listed = scope.listed_subcommands().map(|command| Candidate {
        value: command.name.to_owned(),
        help: command.about,
    });
    let mut candidates: Vec<Candidate> = listed.collect();
    if with_help && scope.command.help_subcommand {
        candidates.push(Candidate {
            value: "help".to_owned(),
            help: help::HELP_ABOUT,
        });
    }
    (!candidates.is_empty()).then_some(Completion::Subcommands(candidates))
}

fn value(arg: &'static Arg, prefix: &str, attached: Option<&str>) -> Completion {
    Completion::Value {
        arg,
        prefix: prefix.to_owned(),
        attached: attached.map(str::to_owned),
    }
}

/// `word` read as no option: the value of the option waiting for one,
/// else of the next positional.
fn plain(level: &Level, word: &str) -> Option<Completion> {
    let at = match (&level.state, &level.pending) {
        (State::Value, Some((at, _))) => Some(*at),
        _ => level.next_positional(),
    }?;
    Some(value(level.scope.slots[at].arg, word, None))
}

/// `word` as the parser will take it only when it is no option here.
fn hyphen_value(level: &Level, word: &str) -> Option<Completion> {
    plain(level, word).filter(|_| level.takes_hyphen_values())
}

/// The options that may still appear, spelled to continue `word`: long
/// names behind `--`, one more letter behind a cluster, both behind `-`.
/// `cluster` holds the flags already in the word.
fn options(level: &Level, word: &str, cluster: &[&'static str]) -> Option<Completion> {
    let scope = level.scope;
    let mut given = level.given();
    if let Some((at, _)) = &level.pending {
        given.push(scope.slots[*at].arg.id);
    }
    given.extend(cluster);
    let mut candidates = Vec::new();
    for arg in scope.slots.iter().map(|slot| slot.arg) {
        let repeats = matches!(arg.kind, Kind::Count | Kind::Multiple);
        if arg.is_positional()
            || (given.contains(&arg.id) && !repeats)
            || !validate::clashes(scope, arg.id, &given).is_empty()
        {
            continue;
        }
        let mut spellings = Vec::new();
        if let Some(short) = arg.short.filter(|_| !word.starts_with("--")) {
            spellings.push(format!("{word}{short}"));
        }
        if word.len() == 1 || word.starts_with("--") {
            let longs = [arg.long, arg.alias].into_iter();
            spellings.extend(
                longs
                    .filter(|name| !name.is_empty())
                    .map(|name| format!("--{name}")),
            );
        }
        candidates.extend(spellings.into_iter().map(|value| Candidate {
            value,
            help: arg.help,
        }));
    }
    (!candidates.is_empty()).then_some(Completion::Options(candidates))
}

/// `--name` or `--name=value` at the cursor.
fn long(level: &Level, word: &str) -> Vec<Completion> {
    let scope = level.scope;
    let (name, attached) = split_long(word);
    let Some(text) = attached else {
        let found = [options(level, word, &[]), hyphen_value(level, word)];
        return found.into_iter().flatten().collect();
    };
    let found = match scope.long(name).map(|at| scope.slots[at].arg) {
        Some(arg) if arg.takes_value() => {
            Some(value(arg, text, Some(&word[..word.len() - text.len()])))
        }
        Some(_) => None,
        None => hyphen_value(level, word),
    };
    found.into_iter().collect()
}

/// `-` or `-abc` at the cursor.
fn short(level: &Level, word: &str) -> Vec<Completion> {
    let scope = level.scope;
    let mut flags = Vec::new();
    for (offset, letter) in scope.cluster(word) {
        match letter {
            Letter::Flag(at) => flags.push(scope.slots[at].arg.id),
            Letter::Valued(at, attached) => {
                let text = attached.unwrap_or("");
                let before = &word[..word.len() - text.len()];
                return vec![value(scope.slots[at].arg, text, Some(before))];
            }
            Letter::Unknown(_) if offset == 0 => {
                return hyphen_value(level, word).into_iter().collect();
            }
            Letter::Unknown(_) => return Vec::new(),
        }
    }
    // a lone `-` is a value to the parser
    let value = plain(level, word).filter(|_| word.len() == 1);
    [options(level, word, &flags), value]
        .into_iter()
        .flatten()
        .collect()
}

fn at_cursor(level: &Level, word: &str) -> Vec<Completion> {
    if level.trailing {
        let at = level.next_positional();
        let found = at.map(|at| value(level.scope.slots[at].arg, word, None));
        return found.into_iter().collect();
    }
    if word.starts_with("--") {
        return long(level, word);
    }
    if word.starts_with('-') {
        return short(level, word);
    }
    let commands = subcommands(level.scope, true).filter(|_| level.state == State::Idle);
    [plain(level, word), commands]
        .into_iter()
        .flatten()
        .collect()
}
