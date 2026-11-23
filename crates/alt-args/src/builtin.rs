//! The arguments every command has.

use crate::arg::Arg;
use crate::settings::Kind;

// what follows the names: value name, help, listed values, hint
pub(crate) const HELP: Arg = Arg::builtin('h', Kind::Help, "help", "\0\0\u{a}Print help\0\0\0");

pub(crate) const VERSION: Arg = Arg::builtin(
    'V',
    Kind::Version,
    "version",
    "\0\0\u{d}Print version\0\0\0",
);
