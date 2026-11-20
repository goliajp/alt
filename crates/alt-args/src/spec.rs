//! The description of a command line.
//!
//! The derive macros write the description of a type as one text
//! ([`Spec::TEXT`]) with no address in it. A table of names, help texts
//! and functions would have to be relocated by the loader at every start
//! of the program, a page of it at the cost of a copy, whichever command
//! is run; a text is read where it lies, and only the part of it that the
//! command line leads to.
//!
//! The text of a type holds its commands: one for a parser or a set of
//! arguments, one for each variant of a set of subcommands.
//!
//! ```text
//! text    = count:2 { name:short offset:3 } { command }
//! command = flags:1 subcommands:2 about:long items
//! items   = count:2 { item }
//! item    = 0 argument | 1 name:short size:3 items | 2 part:2 | 3
//! ```
//!
//! `offset` is where a command begins in the text. `flags` is 1 where
//! `help` is a subcommand. `subcommands` is one more than the part
//! ([`Spec::part`]) that describes the subcommands, 0 for none. Item 1 is
//! a group of arguments recorded under a name, `size` the bytes its items
//! take, item 2 the group of
//! another type, item 3 the place of the subcommands among the arguments.
//! The numbers and strings are those of [`crate::text`]; an argument is
//! laid out as [`Arg`] reads it.

use crate::arg::Arg;
use crate::settings::Words;
use crate::text::Reader;

/// What a type says of the command line it is read from. The derive
/// macros implement it.
pub trait Spec {
    /// the commands of the type
    const TEXT: &'static str;
    /// for each argument by its number, the values its type accepts
    const TYPED: &'static str = "";
    /// printed by `--version`; empty where the flag does not exist
    const VERSION: &'static str = "";

    /// Whether `value` is one the argument of that number takes.
    fn check(_number: usize, _value: &str) -> Result<(), String> {
        Ok(())
    }

    /// The computed default of the argument of that number.
    fn preset(_number: usize) -> String {
        String::new()
    }

    /// Another type whose description this one names by number.
    fn part(_number: usize) -> Node {
        unreachable!("the description names no other type")
    }
}

enum Ask<'a> {
    Text,
    Typed,
    Version,
    Check(usize, &'a str),
    Preset(usize),
    Part(usize),
}

enum Answer {
    Text(&'static str),
    Checked(Result<(), String>),
    Preset(String),
    Part(Node),
}

fn answer<T: Spec>(ask: Ask<'_>) -> Answer {
    match ask {
        Ask::Text => Answer::Text(T::TEXT),
        Ask::Typed => Answer::Text(T::TYPED),
        Ask::Version => Answer::Text(T::VERSION),
        Ask::Check(number, value) => Answer::Checked(T::check(number, value)),
        Ask::Preset(number) => Answer::Preset(T::preset(number)),
        Ask::Part(number) => Answer::Part(T::part(number)),
    }
}

/// A type that describes commands, as a value: one function, taken where
/// it is needed, so that nothing of it stands in a table either.
#[derive(Clone, Copy)]
pub struct Node(fn(Ask<'_>) -> Answer);

impl Node {
    pub fn of<T: Spec>() -> Node {
        Node(answer::<T>)
    }

    fn text(self, ask: Ask<'_>) -> &'static str {
        match (self.0)(ask) {
            Answer::Text(text) => text,
            _ => unreachable!(),
        }
    }

    pub(crate) fn check(self, number: usize, value: &str) -> Result<(), String> {
        match (self.0)(Ask::Check(number, value)) {
            Answer::Checked(checked) => checked,
            _ => unreachable!(),
        }
    }

    pub(crate) fn preset(self, number: usize) -> String {
        match (self.0)(Ask::Preset(number)) {
            Answer::Preset(value) => value,
            _ => unreachable!(),
        }
    }

    fn part(self, number: usize) -> Node {
        match (self.0)(Ask::Part(number)) {
            Answer::Part(node) => node,
            _ => unreachable!(),
        }
    }

    /// The values the type of the argument of that number accepts.
    pub(crate) fn typed(self, number: usize) -> Words {
        let mut reader = Reader::new(self.text(Ask::Typed), 0);
        for _ in 0..number {
            reader.long();
        }
        Words(reader.long())
    }

    /// The commands of the type, in the order of its text.
    fn commands(self) -> Commands {
        let mut reader = Reader::new(self.text(Ask::Text), 0);
        Commands {
            node: self,
            left: reader.two(),
            reader,
        }
    }
}

/// One command: the program itself or a subcommand.
#[derive(Clone, Copy)]
pub struct Command {
    pub name: &'static str,
    /// one line of help, empty for none
    pub about: &'static str,
    /// printed by `--version`; empty where the flag does not exist
    pub version: &'static str,
    /// whether `help [COMMAND]...` is a subcommand
    pub help_subcommand: bool,
    node: Node,
    /// one more than the part that describes the subcommands
    subcommands: usize,
    items: Reader,
}

impl Command {
    /// The command of a parser.
    pub fn of<T: Spec>() -> Command {
        let node = Node::of::<T>();
        let mut command = node.commands().next().expect("a parser is one command");
        command.version = node.text(Ask::Version);
        command
    }

    fn read(node: Node, name: &'static str, offset: usize) -> Command {
        let mut reader = Reader::new(node.text(Ask::Text), offset);
        Command {
            name,
            help_subcommand: reader.one() != 0,
            subcommands: reader.two(),
            about: reader.long(),
            version: "",
            node,
            items: reader,
        }
    }

    pub fn items(&self) -> Items {
        let mut reader = self.items;
        Items {
            node: self.node,
            left: reader.two(),
            reader,
        }
    }

    pub fn subcommands(&self) -> Commands {
        match self.subcommands {
            0 => Commands {
                node: self.node,
                left: 0,
                reader: self.items,
            },
            part => self.node.part(part - 1).commands(),
        }
    }

    /// The subcommand of that name.
    pub fn subcommand(&self, name: &str) -> Option<Command> {
        let mut commands = self.subcommands();
        while commands.left > 0 {
            commands.left -= 1;
            let own = commands.reader.short();
            let offset = commands.reader.three();
            if own == name {
                return Some(Command::read(commands.node, own, offset));
            }
        }
        None
    }
}

/// The commands of one type.
pub struct Commands {
    node: Node,
    left: usize,
    reader: Reader,
}

impl Commands {
    pub fn is_empty(&self) -> bool {
        self.left == 0
    }

    /// How many are left.
    pub fn len(&self) -> usize {
        self.left
    }
}

impl Iterator for Commands {
    type Item = Command;

    fn next(&mut self) -> Option<Command> {
        self.left = self.left.checked_sub(1)?;
        let name = self.reader.short();
        let offset = self.reader.three();
        Some(Command::read(self.node, name, offset))
    }
}

pub enum Item {
    Arg(Arg),
    /// arguments recorded together under a name, or under none when it is
    /// empty; a struct or variant makes one
    Group(&'static str, Items),
    /// where the subcommands sit among the arguments, for help ordering
    Subcommands,
}

/// The arguments of a command or of a group, in the order they were
/// declared in.
pub struct Items {
    node: Node,
    left: usize,
    reader: Reader,
}

impl Iterator for Items {
    type Item = Item;

    fn next(&mut self) -> Option<Item> {
        self.left = self.left.checked_sub(1)?;
        Some(match self.reader.one() {
            0 => Item::Arg(Arg::read(self.node, &mut self.reader)),
            1 => {
                let name = self.reader.short();
                let size = self.reader.three();
                let mut reader = self.reader;
                self.reader.skip(size);
                Item::Group(
                    name,
                    Items {
                        node: self.node,
                        left: reader.two(),
                        reader,
                    },
                )
            }
            2 => {
                let part = self.node.part(self.reader.two());
                let mut command = part.commands().next()?.items();
                command.next()?
            }
            _ => Item::Subcommands,
        })
    }
}
