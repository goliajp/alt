//! Command-line argument parsing from descriptions kept as text.
//!
//! A program describes its command line as a [`Command`]: a tree of
//! subcommands, each with its [`Arg`]s, which a type states as text
//! ([`Spec`]). [`parse`] reads the words of a command line against that
//! tree into [`Matches`], one level per command, and reports anything
//! unacceptable as an [`Error`] whose text names the argument and shows
//! the usage line. `--help` and `--version` come back the same way, with
//! the text to print.
//!
//! The derive macros write the description and the code that reads [`Matches`]
//! into a struct or enum:
//!
//! ```
//! use alt_args::{Parser, Subcommand};
//!
//! #[derive(Parser)]
//! #[command(name = "demo", version)]
//! struct Cli {
//!     #[command(subcommand)]
//!     command: Action,
//!     /// Say more
//!     #[arg(long, short = 'v', global = true)]
//!     verbose: bool,
//! }
//!
//! #[derive(Subcommand)]
//! enum Action {
//!     /// Copy files
//!     Copy {
//!         from: Vec<String>,
//!         #[arg(long, default_value = "out")]
//!         to: String,
//!     },
//! }
//!
//! let words = ["demo", "copy", "a", "b", "-v"].map(String::from);
//! let cli = Cli::try_parse_from(words).unwrap();
//! let Action::Copy { from, to } = cli.command;
//! assert_eq!((from.len(), to.as_str(), cli.verbose), (2, "out", true));
//! ```
//!
//! Accepted spellings: `--name value`, `--name=value`, `-n value`,
//! `-nvalue`, `-n=value`, clustered short flags (`-abc`), and `--` to end
//! option parsing. A single-value option may appear once; an option whose
//! field is a `Vec` may repeat.
//!
//! [`complete::complete`] reads an unfinished command line by the same
//! rules and says what may stand at the cursor, for shell completion.

mod arg;
mod builtin;
pub mod complete;
mod error;
mod help;
mod level;
mod matches;
mod option;
mod parse;
mod scope;
mod settings;
mod spec;
mod suggest;
mod text;
mod usage;
mod validate;
mod value;

pub use alt_args_derive::{Args, Parser, Subcommand, ValueEnum};
pub use arg::Arg;
pub use error::{Error, ErrorKind};
pub use matches::{Matched, Matches};
pub use parse::parse;
pub use settings::{Computed, Hint, Kind, Preset, Rare, Words};
pub use spec::{Command, Commands, Item, Items, Node, Spec};
pub use text::{joined, joined_len};
pub use value::{Check, Value, check_as};

/// A whole command line.
pub trait Parser: Spec + Sized {
    fn from_matches(matches: &Matches) -> Self;

    /// The command the type describes.
    fn command() -> Command {
        Command::of::<Self>()
    }

    /// Parses `words`, the first of which is the program path.
    fn try_parse_from(words: impl IntoIterator<Item = String>) -> Result<Self, Error> {
        parse(Self::command(), words).map(|matches| Self::from_matches(&matches))
    }

    /// Parses the arguments of the process. Help, version and errors are
    /// printed and end the process; an argument that is not UTF-8 is an
    /// error.
    fn parse() -> Self {
        let words: Result<Vec<String>, _> =
            std::env::args_os().map(|word| word.into_string()).collect();
        let parsed = match words {
            Ok(words) => Self::try_parse_from(words),
            Err(word) => Err(Error::new(
                ErrorKind::Usage,
                format!("error: invalid UTF-8 was found in argument {word:?}\n"),
            )),
        };
        parsed.unwrap_or_else(|error| error.exit())
    }
}

/// A set of arguments that can be spliced into a command.
pub trait Args: Spec + Sized {
    fn from_matches(matches: &Matches) -> Self;
}

/// A choice between subcommands.
pub trait Subcommand: Spec + Sized {
    /// Reads the level of the chosen subcommand.
    fn from_matches(matches: &Matches) -> Self;
}
