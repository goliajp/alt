//! The words of one command level: options, their values and positionals.

use crate::error::{Error, ErrorKind};
use crate::matches::Matched;
use crate::scope::Scope;
use crate::settings::Kind;
use crate::spec::Command;
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

/// What reading one word led to.
pub(crate) enum Step {
    /// the word belongs to this level
    Taken,
    /// the word is `help`; the words after it name a command
    Help,
    /// the word names a subcommand; the words after it are its own
    Subcommand(Command),
}

impl<'a> Level<'a> {
    pub(crate) fn new(scope: &'a Scope) -> Self {
        Level {
            scope,
            entries: Vec::new(),
            pending: None,
            state: State::Idle,
            filled: 0,
            trailing: false,
        }
    }

    /// Reads the next word; `rest` is what follows it on the line.
    pub(crate) fn word(&mut self, word: &str, rest: &[String]) -> Result<Step, Error> {
        if !self.trailing {
            let command = self.scope.command;
            if self.state == State::Idle {
                if word == "help" && command.help_subcommand {
                    return Ok(Step::Help);
                }
                if let Some(command) = command.subcommand(word) {
                    return Ok(Step::Subcommand(command));
                }
            }
            if word == "--" {
                self.trailing = true;
                return Ok(Step::Taken);
            }
            let is_option = if word.starts_with("--") {
                self.long(word, rest)?
            } else if word.starts_with('-') && word.len() > 1 {
                self.short(word)?
            } else {
                false
            };
            if is_option {
                return Ok(Step::Taken);
            }
            if self.state == State::Value {
                if let Some((_, values)) = &mut self.pending {
                    values.push(word.to_owned());
                }
                self.state = State::Idle;
                return Ok(Step::Taken);
            }
        }
        self.positional(word.to_owned())?;
        Ok(Step::Taken)
    }

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
    pub(crate) fn record(&mut self, at: usize, values: Vec<String>) -> Result<(), Error> {
        let slot = &self.scope.slots[at];
        let arg = &slot.arg;
        if values.is_empty() {
            return Err(Error::missing_value(arg));
        }
        let seen = self.entries.iter().any(|entry| entry.id == arg.id);
        if seen && !matches!(arg.kind, Kind::Multiple | Kind::Count | Kind::Attached) {
            return Err(Error::repeated(self.scope, arg));
        }
        self.entry(arg.id);
        for group in [slot.owner, arg.group()] {
            if !group.is_empty() {
                self.entry(group).values.push(arg.id.to_owned());
            }
        }
        for value in values {
            if let Err(reason) = arg.check(&value) {
                let possible = arg.possible();
                if possible.is_empty() || possible.contains(&value) {
                    return Err(Error::invalid_value(&value, arg, &reason));
                }
                let close = similar(&value, possible.iter()).pop();
                return Err(Error::not_possible(&value, arg, close));
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
    pub(crate) fn next_positional(&self) -> Option<usize> {
        self.scope.positionals().nth(self.filled).map(|(at, _)| at)
    }

    pub(crate) fn takes_hyphen_values(&self) -> bool {
        self.next_positional()
            .is_some_and(|at| self.scope.slots[at].arg.hyphen_values)
    }

    /// The help or version text, which ends parsing like an error.
    pub(crate) fn builtin(&self, kind: Kind) -> Error {
        let command = self.scope.command;
        match kind {
            Kind::Version => Error::new(
                ErrorKind::Version,
                format!("{} {}\n", command.name, command.version),
            ),
            _ => Error::new(ErrorKind::Help, help::render(self.scope)),
        }
    }

    fn positional(&mut self, word: String) -> Result<(), Error> {
        let Some(at) = self.next_positional() else {
            let _ = self.settle();
            return Err(self.unexpected(&word));
        };
        let arg = &self.scope.slots[at].arg;
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
        if self.scope.has_subcommands() {
            let close = similar(word, self.scope.listed_subcommand_names());
            if !close.is_empty() || !self.scope.has_positionals() {
                return Error::unknown_subcommand(self.scope, word, &close);
            }
        }
        Error::unknown_argument(word, &[], usage::full(self.scope))
    }
}
