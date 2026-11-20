//! The description the derive macros write, read back through
//! [`Command`]: every setting of an argument, the arguments of another
//! type spliced in, and the subcommands in the order they were declared.

use alt_args::{Arg, Args, Command, Item, Kind, Parser, Preset, Subcommand, ValueEnum};

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq)]
enum Color {
    Auto,
    Never,
}

fn even(text: &str) -> Result<u32, String> {
    match text.parse::<u32>() {
        Ok(number) if number % 2 == 0 => Ok(number),
        _ => Err("not even".to_owned()),
    }
}

/// Paints things
#[derive(Parser)]
#[command(name = "paint", version)]
struct Cli {
    #[command(subcommand)]
    command: Action,
    /// Say more
    #[arg(long, short = 'v', global = true)]
    verbose: bool,
    /// When to colour
    #[arg(long, value_name = "WHEN", default_value = "auto")]
    color: Color,
}

#[derive(Args)]
struct Size {
    /// How wide
    #[arg(long, default_value_t = 2 + 2, conflicts_with = "tall")]
    wide: u32,
    #[arg(long, visible_alias = "high", requires = "wide", group = "extent")]
    tall: Option<u32>,
}

#[derive(Subcommand)]
enum Action {
    /// Draws a line
    Line {
        #[command(flatten)]
        size: Size,
        #[arg(long, value_parser = even)]
        dashes: Option<u32>,
        #[arg(long, value_parser = ["round", "square"], default_missing_value = "round")]
        cap: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        points: Vec<String>,
    },
    Fill(Size),
    Clear,
}

fn args(command: &Command) -> Vec<(&'static str, Arg)> {
    fn walk(items: alt_args::Items, group: &'static str, found: &mut Vec<(&'static str, Arg)>) {
        for item in items {
            match item {
                Item::Arg(arg) => found.push((group, arg)),
                Item::Group(name, items) => walk(items, name, found),
                Item::Subcommands => {}
            }
        }
    }
    let mut found = Vec::new();
    walk(command.items(), "", &mut found);
    found
}

#[test]
fn a_parser_describes_itself_and_where_its_subcommands_are() {
    let paint = Cli::command();
    assert_eq!(
        (paint.name, paint.about, paint.help_subcommand),
        ("paint", "Paints things", true)
    );
    assert_eq!(paint.version, env!("CARGO_PKG_VERSION"));
    let own = args(&paint);
    let ids: Vec<_> = own.iter().map(|(group, arg)| (*group, arg.id)).collect();
    assert_eq!(ids, [("Cli", "verbose"), ("Cli", "color")]);
    let verbose = &own[0].1;
    assert_eq!((verbose.long, verbose.short), ("verbose", Some('v')));
    assert_eq!((verbose.help(), verbose.global), ("Say more", true));
    assert!(verbose.kind == Kind::Flag && !verbose.required);
    assert!(verbose.possible().is_empty());
    let color = &own[1].1;
    assert_eq!(
        (color.value_name(), color.help()),
        ("WHEN", "When to colour")
    );
    assert!(color.kind == Kind::Single && !color.global && !color.required);
    assert_eq!(color.possible().to_vec(), ["auto", "never"]);
    assert!(matches!(color.preset(), Preset::Text("auto")));
    let names: Vec<_> = paint.subcommands().map(|c| (c.name, c.about)).collect();
    assert_eq!(
        names,
        [("line", "Draws a line"), ("fill", ""), ("clear", "")]
    );
    assert_eq!(paint.subcommands().len(), 3);
    assert!(paint.subcommand("lines").is_none());
    assert!(paint.subcommand("clear").unwrap().items().next().is_none());
    assert!(paint.subcommand("clear").unwrap().subcommands().is_empty());
}

#[test]
fn a_variant_describes_its_arguments_and_those_of_the_type_it_splices_in() {
    let line = Cli::command().subcommand("line").unwrap();
    assert_eq!((line.version, line.help_subcommand), ("", false));
    let own = args(&line);
    let ids: Vec<_> = own.iter().map(|(group, arg)| (*group, arg.id)).collect();
    // a variant that splices arguments in records its own under no name
    assert_eq!(
        ids,
        [
            ("Size", "wide"),
            ("Size", "tall"),
            ("", "dashes"),
            ("", "cap"),
            ("", "points")
        ]
    );
    let by = |id: &str| own.iter().find(|(_, arg)| arg.id == id).unwrap().1;
    let wide = by("wide").rare();
    let Preset::Computed(computed) = wide.preset else {
        panic!("a default that is computed");
    };
    assert_eq!(computed.value(), "4");
    assert_eq!(wide.conflicts.to_vec(), ["tall"]);
    let tall = by("tall").rare();
    assert_eq!((tall.alias, tall.group), ("high", "extent"));
    assert_eq!(tall.requires.to_vec(), ["wide"]);
    assert!(matches!(tall.preset, Preset::None) && tall.conflicts.is_empty());
    let cap = by("cap");
    assert_eq!(cap.possible().to_vec(), ["round", "square"]);
    assert_eq!((cap.missing(), cap.alias()), ("round", ""));
    let points = by("points");
    assert!(points.kind == Kind::Multiple && points.trailing && points.hyphen_values);
    assert!(points.long.is_empty() && points.short.is_none());
    assert!(points.alias().is_empty() && points.conflicts().is_empty());
    // a variant that holds a type of arguments has that type's group
    let fill = Cli::command().subcommand("fill").unwrap();
    let ids: Vec<_> = args(&fill).iter().map(|(g, arg)| (*g, arg.id)).collect();
    assert_eq!(ids, [("Size", "wide"), ("Size", "tall")]);
}

#[test]
fn values_are_checked_by_the_type_or_the_function_the_argument_names() {
    let parse = |words: &[&str]| {
        let words = ["paint"].iter().chain(words).map(|word| word.to_string());
        Cli::try_parse_from(words).map_err(|error| error.to_string())
    };
    let cli = parse(&["line", "--wide", "7", "--dashes", "4", "--cap", "-v", "-x"]).unwrap();
    let Action::Line {
        size,
        dashes,
        cap,
        points,
    } = cli.command
    else {
        panic!("a line");
    };
    assert_eq!((size.wide, size.tall, dashes), (7, None, Some(4)));
    assert_eq!((cap.as_deref(), cli.verbose), (Some("round"), true));
    assert_eq!(points, ["-x"]);
    let Action::Fill(size) = parse(&["fill", "--tall", "3"]).unwrap().command else {
        panic!("a fill");
    };
    assert_eq!((size.wide, size.tall), (4, Some(3)));
    assert!(matches!(parse(&["clear"]).unwrap().command, Action::Clear));
    let odd = parse(&["line", "--dashes", "3"]).err().unwrap();
    assert!(
        odd.contains("invalid value '3' for '--dashes <DASHES>': not even"),
        "{odd}"
    );
    let word = parse(&["line", "--wide", "x"]).err().unwrap();
    assert!(
        word.contains("invalid value 'x' for '--wide <WIDE>'"),
        "{word}"
    );
    let cap = parse(&["line", "--cap=flat"]).err().unwrap();
    assert!(cap.contains("[possible values: round, square]"), "{cap}");
    let color = parse(&["--color", "nevr", "clear"]).err().unwrap();
    assert!(
        color.contains("invalid value 'nevr' for '--color <WHEN>'"),
        "{color}"
    );
    assert!(color.contains("[possible values: auto, never]"), "{color}");
    assert!(color.contains("a similar value exists: 'never'"), "{color}");
    assert_eq!(
        parse(&["--color", "never", "clear"]).unwrap().color,
        Color::Never
    );
}
