//! A flag with `action = count` may be given several times; the field
//! holds how often it was.

use alt_args::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tool")]
struct Tool {
    #[command(subcommand)]
    command: Op,
}

#[derive(Subcommand)]
enum Op {
    /// Look for something
    Look {
        /// look harder, each time it is given
        #[arg(short = 'C', long = "copies", action = count)]
        copies: u8,
        #[arg(short = 'v')]
        verbose: bool,
        path: String,
    },
}

fn look(words: &[&str]) -> Result<(u8, bool, String), String> {
    let words = ["tool", "look"].iter().chain(words);
    let Op::Look {
        copies,
        verbose,
        path,
    } = Tool::try_parse_from(words.map(|w| (*w).to_owned()))
        .map_err(|e| e.to_string())?
        .command;
    Ok((copies, verbose, path))
}

#[test]
fn a_counted_flag_counts_its_appearances() {
    assert_eq!(look(&["file"]).unwrap().0, 0);
    assert_eq!(look(&["-C", "file"]).unwrap().0, 1);
    assert_eq!(
        look(&["-C", "-v", "--copies", "file", "-C"]).unwrap(),
        (3, true, "file".to_owned())
    );
    // an ordinary flag still may not repeat
    assert!(look(&["-v", "-v", "file"]).is_err());
}

#[derive(Parser)]
#[command(name = "tool")]
struct Tracer {
    #[command(subcommand)]
    command: Step,
}

#[derive(Subcommand)]
enum Step {
    /// Trace something
    Trace {
        /// follow moved lines; a number against the letter sets how many
        #[arg(short = 'M', action = attached)]
        moves: Vec<String>,
        #[arg(short = 'v')]
        verbose: bool,
        path: String,
    },
}

#[test]
fn an_attached_flag_keeps_what_is_written_against_its_letter() {
    let trace = |words: &[&str]| {
        let words = ["tool", "trace"].iter().chain(words);
        let Step::Trace {
            moves,
            verbose,
            path,
        } = Tracer::try_parse_from(words.map(|w| (*w).to_owned()))
            .map_err(|e| e.to_string())?
            .command;
        Ok::<_, String>((moves, verbose, path))
    };
    assert!(trace(&["file"]).unwrap().0.is_empty());
    assert_eq!(trace(&["-M", "file"]).unwrap().0, [""]);
    assert_eq!(trace(&["-M40", "file"]).unwrap().0, ["40"]);
    // a bare one takes nothing from the next word, and may be repeated
    let (moves, verbose, path) = trace(&["-M", "-vM7", "file"]).unwrap();
    assert_eq!(moves, ["", "7"]);
    assert!(verbose && path == "file");
    assert_eq!(trace(&["-Mv", "file"]).unwrap().0, ["v"]);
}
