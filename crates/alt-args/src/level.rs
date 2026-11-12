//! The words of one command level: options, their values and positionals.

use crate::error::{Error, ErrorKind};
use crate::matches::Matched;
use crate::scope::Scope;
use crate::spec::Kind;
use crate::suggest::similar;
use crate::{help, usage, validate};

#[derive(PartialEq)]
pub(crate) enum State {
    Idle,
    /// an option is waiting for its value
    Value,
    /// a positional that takes any number of values is open
    Positional,
}

pub(crate) struct Level<'a> {
    pub(crate) scope: &'a Scope,
    pub(crate) entries: Vec<Matched>,
    /// values read for a slot but not yet checked and recorded
    pub(crate) pending: Option<(usize, Vec<String>)>,
    pub(crate) state: State,
    /// how many single-value positionals are filled
    pub(crate) filled: usize,
    /// every further word is a positional value
    pub(crate) trailing: bool,
}

impl Level<'_> {
    fn entry(&mut self, id: &'static str) -> &mut Matched {
        let at = self.entries.iter().position(|entry| entry.id == id);
        let at = at.unwrap_or_else(|| {
            self.entries.push(Matched {
                id,
                values: Vec::new(),
                explicit: true,
            });
            self.entries.len() - 1
        });
        &mut self.entries[at]
    }

    /// Records one appearance of a slot with its values.
    fn record(&mut self, at: usize, values: Vec<String>) -> Result<(), Error> {
        let slot = &self.scope.slots[at];
        let arg = slot.arg;
        if values.is_empty() {
            return Err(Error::missing_value(arg));
        }
        let seen = self.entries.iter().any(|entry| entry.id == arg.id);
        if seen && arg.kind != Kind::Multiple {
            return Err(Error::repeated(self.scope, arg));
        }
        self.entry(arg.id);
        for group in [slot.owner, arg.group] {
            if !group.is_empty() {
                self.entry(group).values.push(arg.id.to_owned());
            }
        }
        for value in values {
            if !arg.possible.is_empty() && !arg.possible.contains(&value.as_str()) {
                let close = similar(&value, arg.possible.iter().copied()).pop();
                return Err(Error::not_possible(&value, arg, close));
            }
            if let Some(Err(reason)) = arg.check.map(|check| check(&value)) {
                return Err(Error::invalid_value(&value, arg, &reason));
            }
            self.entry(arg.id).values.push(value);
        }
        Ok(())
    }

    pub(crate) fn settle(&mut self) -> Result<(), Error> {
        self.state = State::Idle;
        match self.pending.take() {
            Some((at, values)) => self.record(at, values),
            None => Ok(()),
        }
    }

    pub(crate) fn given(&self) -> Vec<&str> {
        validate::given(self.scope, &self.entries)
    }

    /// The positional the next plain word goes to.
    fn next_positional(&self) -> Option<usize> {
        self.scope.positionals().nth(self.filled).map(|(at, _)| at)
    }

    fn takes_hyphen_values(&self) -> bool {
        self.next_positional()
            .is_some_and(|at| self.scope.slots[at].arg.hyphen_values)
    }

    /// The help or version text, which ends parsing like an error.
    fn builtin(&self, kind: Kind) -> Error {
        let command = self.scope.command;
        match kind {
            Kind::Version => Error::new(
                ErrorKind::Version,
                format!("{} {}\n", command.name, command.version),
            ),
            _ => Error::new(ErrorKind::Help, help::render(self.scope)),
        }
    }

    fn pass_as_value(&self, word: &str) -> Option<String> {
        (!self.trailing && self.scope.has_positionals())
            .then(|| format!("to pass '{word}' as a value, use '-- {word}'"))
    }

    /// `--name` or `--name=value`; false when the word is a value instead.
    pub(crate) fn long(&mut self, word: &str, rest: &[String]) -> Result<bool, Error> {
        let (name, attached) = match word[2..].split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (&word[2..], None),
        };
        let Some(at) = self.scope.long(name) else {
            return match self.takes_hyphen_values() {
                true => Ok(false),
                false => Err(self.unknown_long(name, rest)),
            };
        };
        let arg = self.scope.slots[at].arg;
        if arg.is_builtin() {
            self.settle()?;
            return Err(self.builtin(arg.kind));
        }
        match (arg.takes_value(), attached) {
            (false, Some(value)) => {
                let mut used = self.given();
                used.retain(|id| self.scope.find(id).is_some_and(|slot| !slot.arg.required));
                used.push(arg.id);
                let usage = usage::narrowed(self.scope, &usage::required(self.scope), &used);
                return Err(Error::unexpected_value(value, arg, usage));
            }
            (false, None) => {
                self.settle()?;
                self.record(at, vec!["true".to_owned()])?;
            }
            (true, Some(value)) => {
                self.settle()?;
                self.record(at, vec![value.to_owned()])?;
            }
            (true, None) => {
                self.settle()?;
                self.pending = Some((at, Vec::new()));
                self.state = State::Value;
            }
        }
        Ok(true)
    }

    fn unknown_long(&mut self, name: &str, rest: &[String]) -> Error {
        let _ = self.settle();
        let word = format!("--{name}");
        let scope = self.scope;
        let mut used = self.given();
        let mut tips = Vec::new();
        if let Some(close) = similar(name, scope.longs()).pop() {
            tips.push(format!("a similar argument exists: '--{close}'"));
            if let Some(at) = scope.long(close) {
                used.push(scope.slots[at].arg.id);
            }
        } else if let Some((command, close)) = in_subcommand(scope, name, rest) {
            tips.push(format!("'{command} --{close}' exists"));
        } else {
            tips.extend(self.pass_as_value(&word));
        }
        let usage = usage::narrowed(scope, &usage::required(scope), &used);
        Error::unknown_argument(&word, &tips, usage)
    }

    /// `-abc`, where a letter that takes a value ends the cluster.
    pub(crate) fn short(&mut self, word: &str) -> Result<bool, Error> {
        let cluster = &word[1..];
        for (offset, letter) in cluster.char_indices() {
            let Some(at) = self.scope.short(letter) else {
                if offset == 0 && self.takes_hyphen_values() {
                    return Ok(false);
                }
                let _ = self.settle();
                let word = format!("-{letter}");
                let tips: Vec<String> = self.pass_as_value(&word).into_iter().collect();
                return Err(Error::unknown_argument(
                    &word,
                    &tips,
                    usage::full(self.scope),
                ));
            };
            self.settle()?;
            let arg = self.scope.slots[at].arg;
            if arg.is_builtin() {
                return Err(self.builtin(arg.kind));
            }
            if !arg.takes_value() {
                self.record(at, vec!["true".to_owned()])?;
                continue;
            }
            let attached = &cluster[offset + letter.len_utf8()..];
            if attached.is_empty() {
                self.pending = Some((at, Vec::new()));
                self.state = State::Value;
            } else {
                let value = attached.strip_prefix('=').unwrap_or(attached);
                self.record(at, vec![value.to_owned()])?;
            }
            break;
        }
        Ok(true)
    }

    pub(crate) fn positional(&mut self, word: String) -> Result<(), Error> {
        let Some(at) = self.next_positional() else {
            let _ = self.settle();
            return Err(self.unexpected(&word));
        };
        let arg = self.scope.slots[at].arg;
        self.trailing |= arg.trailing;
        match &mut self.pending {
            Some((open, values)) if *open == at && arg.kind == Kind::Multiple => values.push(word),
            _ => {
                self.settle()?;
                self.pending = Some((at, vec![word]));
            }
        }
        if arg.kind == Kind::Multiple {
            self.state = State::Positional;
        } else {
            self.filled += 1;
            self.state = State::Idle;
        }
        Ok(())
    }

    /// A word no positional is left for.
    fn unexpected(&self, word: &str) -> Error {
        let names = self.scope.subcommand_names();
        if !names.is_empty() {
            let close = similar(word, names.iter().copied());
            if !close.is_empty() || !self.scope.has_positionals() {
                return Error::unknown_subcommand(self.scope, word, &close);
            }
        }
        Error::unknown_argument(word, &[], usage::full(self.scope))
    }
}

/// A subcommand named later on the line that has a long option close to
/// `name`.
fn in_subcommand(
    scope: &Scope,
    name: &str,
    rest: &[String],
) -> Option<(&'static str, &'static str)> {
    let mut best: Option<(usize, &'static str, &'static str)> = None;
    for command in scope.command.subcommands {
        let Some(at) = rest.iter().position(|word| word == command.name) else {
            continue;
        };
        let Some(close) = similar(name, scope.child(command).longs()).pop() else {
            continue;
        };
        if best.is_none_or(|(other, _, _)| at < other) {
            best = Some((at, command.name, close));
        }
    }
    best.map(|(_, command, close)| (command, close))
}
