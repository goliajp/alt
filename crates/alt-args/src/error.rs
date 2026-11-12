//! Parse failures and the help and version texts, which stop parsing the
//! same way.

use std::fmt;

use crate::scope::Scope;
use crate::spec::Arg;
use crate::usage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// `--help` was asked for; the message is the help text
    Help,
    /// `--version` was asked for; the message is the version line
    Version,
    /// the command line is not acceptable
    Usage,
}

#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Prints the message and ends the process: help and version on
    /// standard output with status 0, anything else on standard error
    /// with status 2.
    pub fn exit(&self) -> ! {
        if self.kind == ErrorKind::Usage {
            eprint!("{self}");
            std::process::exit(2);
        }
        print!("{self}");
        std::process::exit(0);
    }

    pub(crate) fn new(kind: ErrorKind, message: String) -> Self {
        Error { kind, message }
    }

    fn usage(body: String, tips: &[String], usage: Option<String>) -> Self {
        let mut message = format!("error: {body}\n");
        for tip in tips {
            message += &format!("\n  tip: {tip}\n");
        }
        if let Some(usage) = usage {
            message += &format!("\n{usage}\n");
        }
        message += "\nFor more information, try '--help'.\n";
        Error::new(ErrorKind::Usage, message)
    }

    pub(crate) fn unknown_argument(word: &str, tips: &[String], usage: String) -> Self {
        Error::usage(
            format!("unexpected argument '{word}' found"),
            tips,
            Some(usage),
        )
    }

    pub(crate) fn unknown_subcommand(scope: &Scope, word: &str, similar: &[&str]) -> Self {
        let quoted: Vec<String> = similar.iter().map(|name| format!("'{name}'")).collect();
        let tips = match quoted.as_slice() {
            [] => Vec::new(),
            [one] => vec![format!("a similar subcommand exists: {one}")],
            many => vec![format!(
                "some similar subcommands exist: {}",
                many.join(", ")
            )],
        };
        Error::usage(
            format!("unrecognized subcommand '{word}'"),
            &tips,
            Some(usage::full(scope)),
        )
    }

    pub(crate) fn missing_subcommand(scope: &Scope) -> Self {
        Error::usage(
            format!(
                "'{}' requires a subcommand but one was not provided\n  [subcommands: {}]",
                scope.path,
                scope.subcommand_names().join(", ")
            ),
            &[],
            Some(usage::full(scope)),
        )
    }

    pub(crate) fn unexpected_value(value: &str, arg: &Arg, usage: String) -> Self {
        Error::usage(
            format!(
                "unexpected value '{value}' for '{}' found; no more were expected",
                arg.display(None)
            ),
            &[],
            Some(usage),
        )
    }

    pub(crate) fn repeated(scope: &Scope, arg: &Arg) -> Self {
        Error::usage(
            format!(
                "the argument '{}' cannot be used multiple times",
                arg.display(None)
            ),
            &[],
            Some(usage::full(scope)),
        )
    }

    fn possible(arg: &Arg) -> String {
        if arg.possible.is_empty() {
            return String::new();
        }
        format!("\n  [possible values: {}]", arg.possible.join(", "))
    }

    pub(crate) fn missing_value(arg: &Arg) -> Self {
        Error::usage(
            format!(
                "a value is required for '{}' but none was supplied{}",
                arg.display(None),
                Error::possible(arg)
            ),
            &[],
            None,
        )
    }

    pub(crate) fn not_possible(value: &str, arg: &Arg, similar: Option<&str>) -> Self {
        let tip = similar.map(|name| format!("a similar value exists: '{name}'"));
        Error::usage(
            format!(
                "invalid value '{value}' for '{}'{}",
                arg.display(None),
                Error::possible(arg)
            ),
            tip.as_slice(),
            None,
        )
    }

    pub(crate) fn invalid_value(value: &str, arg: &Arg, reason: &str) -> Self {
        Error::usage(
            format!(
                "invalid value '{value}' for '{}': {reason}",
                arg.display(None)
            ),
            &[],
            None,
        )
    }

    pub(crate) fn conflict(arg: &Arg, others: &[String], usage: String) -> Self {
        let body = match others {
            [one] => format!(
                "the argument '{}' cannot be used with '{one}'",
                arg.display(None)
            ),
            many => format!(
                "the argument '{}' cannot be used with:\n  {}",
                arg.display(None),
                many.join("\n  ")
            ),
        };
        Error::usage(body, &[], Some(usage))
    }

    pub(crate) fn missing_arguments(missing: &[String], usage: String) -> Self {
        Error::usage(
            format!(
                "the following required arguments were not provided:\n  {}",
                missing.join("\n  ")
            ),
            &[],
            Some(usage),
        )
    }
}
