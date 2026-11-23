//! Reading a command line against a command and its subcommands.

use crate::error::{Error, ErrorKind};
use crate::level::{Level, Step};
use crate::matches::{Matched, Matches};
use crate::scope::Scope;
use crate::spec::Command;
use crate::{help, validate};

/// `help [COMMAND]...`: the help of the named command.
fn help_subcommand(scope: &Scope, names: &[String]) -> Error {
    let Some((name, rest)) = names.split_first() else {
        return Error::new(ErrorKind::Help, help::render(scope));
    };
    match scope.command.subcommand(name) {
        Some(command) => help_subcommand(&scope.child(command), rest),
        None if name == "help" && scope.command.help_subcommand => {
            Error::new(ErrorKind::Help, help::of_help(scope))
        }
        None => Error::unknown_subcommand(scope, name, &[]),
    }
}

fn level(scope: &Scope, words: &[String], at: &mut usize) -> Result<Matches, Error> {
    let mut level = Level::new(scope);
    let mut subcommand = None;
    while let Some(word) = words.get(*at) {
        *at += 1;
        match level.word(word, &words[*at..])? {
            Step::Taken => {}
            Step::Help => return Err(help_subcommand(scope, &words[*at..])),
            Step::Subcommand(command) => {
                level.settle()?;
                subcommand = Some(Box::new(self::level(&scope.child(command), words, at)?));
                break;
            }
        }
    }
    level.settle()?;
    if subcommand.is_none() && scope.has_subcommands() {
        return Err(match level.given().is_empty() {
            true => Error::new(ErrorKind::Usage, help::render(scope)),
            false => Error::missing_subcommand(scope),
        });
    }
    let (dashed, dashes_alone) = (level.dashed, level.dashes_alone());
    validate::finish(scope, &mut level.entries)?;
    let own = scope.slots[..scope.own].iter();
    Ok(Matches {
        name: scope.command.name,
        globals: own
            .filter(|slot| slot.arg.global)
            .map(|slot| slot.arg.id)
            .collect(),
        entries: level.entries,
        subcommand,
        dashed,
        dashes_alone,
    })
}

/// Gives every level the same value for each global argument: the one
/// given deepest on the command line, else the default.
fn spread(matches: &mut Matches, ids: &mut Vec<&'static str>, values: &mut Vec<Matched>) {
    ids.extend(&matches.globals);
    for id in ids.iter() {
        let Some(own) = matches.entry(id) else {
            continue;
        };
        match values.iter_mut().find(|value| value.id == *id) {
            Some(value) if value.explicit && !own.explicit => {}
            Some(value) => *value = own.clone(),
            None => values.push(own.clone()),
        }
    }
    if let Some(subcommand) = &mut matches.subcommand {
        spread(subcommand, ids, values);
    }
    for value in values.iter() {
        matches.upsert(value.clone());
    }
}

/// Parses `words`, the first of which is the program path.
pub fn parse(command: Command, words: impl IntoIterator<Item = String>) -> Result<Matches, Error> {
    let words: Vec<String> = words.into_iter().collect();
    let program = words.first().and_then(|path| {
        let name = std::path::Path::new(path).file_name()?.to_str()?;
        Some(name.to_owned())
    });
    let scope = Scope::root(command, program.unwrap_or_else(|| command.name.to_owned()));
    let mut matches = level(&scope, &words, &mut 1)?;
    spread(&mut matches, &mut Vec::new(), &mut Vec::new());
    Ok(matches)
}
