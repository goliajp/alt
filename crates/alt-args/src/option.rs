//! The words of a command level that begin with `-`.

use crate::error::Error;
use crate::level::{Level, State};
use crate::scope::{Letter, Scope, split_long};
use crate::suggest::similar;
use crate::usage;

impl Level<'_> {
    fn pass_as_value(&self, word: &str) -> Option<String> {
        (!self.trailing && self.scope.has_positionals())
            .then(|| format!("to pass '{word}' as a value, use '-- {word}'"))
    }

    /// `--name` or `--name=value`; false when the word is a value instead.
    pub(crate) fn long(&mut self, word: &str, rest: &[String]) -> Result<bool, Error> {
        let (name, attached) = split_long(word);
        let Some(at) = self.scope.long(name) else {
            return match self.takes_hyphen_values() {
                true => Ok(false),
                false => Err(self.unknown_long(name, rest)),
            };
        };
        let arg = &self.scope.slots[at].arg;
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
            (true, None) if !arg.missing().is_empty() => {
                self.settle()?;
                self.record(at, vec![arg.missing().to_owned()])?;
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
        for (offset, letter) in self.scope.cluster(word) {
            let at = match letter {
                Letter::Unknown(letter) => {
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
                }
                Letter::Flag(at) | Letter::Attached(at, _) | Letter::Valued(at, _) => at,
            };
            self.settle()?;
            let arg = &self.scope.slots[at].arg;
            if arg.is_builtin() {
                return Err(self.builtin(arg.kind));
            }
            match letter {
                // nothing against the letter is told apart from any value
                Letter::Attached(_, attached) => self.record(at, vec![attached.to_owned()])?,
                Letter::Valued(_, Some(value)) => self.record(at, vec![value.to_owned()])?,
                Letter::Valued(_, None) if !arg.missing().is_empty() => {
                    self.record(at, vec![arg.missing().to_owned()])?
                }
                Letter::Valued(_, None) => {
                    self.pending = Some((at, Vec::new()));
                    self.state = State::Value;
                }
                _ => self.record(at, vec!["true".to_owned()])?,
            }
        }
        Ok(true)
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
    for command in scope.listed_subcommands() {
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
