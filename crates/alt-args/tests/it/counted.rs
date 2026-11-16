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
