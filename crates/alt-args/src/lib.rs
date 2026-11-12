//! Command-line argument parsing from static tables.
//!
//! A program describes its command line as a [`Command`]: a constant tree
//! of subcommands, each with a table of [`Arg`]s. [`parse`] reads the words
//! of a command line against that tree into [`Matches`], one level per
//! command, and reports anything unacceptable as an [`Error`] whose text
//! names the argument and shows the usage line. `--help` and `--version`
//! come back the same way, with the text to print.
//!
//! The derive macros write the tables and the code that reads [`Matches`]
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

mod error;
mod help;
mod level;
mod matches;
mod parse;
mod scope;
mod spec;
mod suggest;
mod usage;
mod validate;
mod value;

pub use alt_args_derive::{Args, Parser, Subcommand, ValueEnum};
pub use error::{Error, ErrorKind};
pub use matches::{Matched, Matches};
pub use parse::parse;
pub use spec::{Arg, Command, Item, Kind, Preset};
pub use value::{Check, Value};

/// A whole command line.
pub trait Parser: Sized {
    const COMMAND: &'static Command;

    fn from_matches(matches: &Matches) -> Self;

    /// Parses `words`, the first of which is the program path.
    fn try_parse_from(words: impl IntoIterator<Item = String>) -> Result<Self, Error> {
        parse(Self::COMMAND, words).map(|matches| Self::from_matches(&matches))
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
pub trait Args: Sized {
    /// the arguments, grouped under the type's name
    const GROUP: Item;

    fn from_matches(matches: &Matches) -> Self;
}

/// A choice between subcommands.
pub trait Subcommand: Sized {
    const COMMANDS: &'static [Command];

    /// Reads the level of the chosen subcommand.
    fn from_matches(matches: &Matches) -> Self;
}
