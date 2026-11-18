//! An option with `default_missing_value` may be given without a value;
//! it then takes one only when attached, never the next word.

use alt_args::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tool")]
struct Tool {
    #[command(subcommand)]
    command: Op,
}

#[derive(Subcommand)]
enum Op {
    /// Show something
    Show {
        /// how much to show
        #[arg(
            short = 'u',
            long = "untracked-files",
            value_name = "MODE",
            default_missing_value = "all",
            value_parser = ["no", "normal", "all"]
        )]
        untracked: Option<String>,
        #[arg(short = 'v')]
        verbose: bool,
        path: Option<String>,
    },
}

type Shown = (Option<String>, bool, Option<String>);

fn show(words: &[&str]) -> Result<Shown, String> {
    let words = ["tool", "show"].iter().chain(words);
    let Op::Show {
        untracked,
        verbose,
        path,
    } = Tool::try_parse_from(words.map(|w| (*w).to_owned()))
        .map_err(|e| e.to_string())?
        .command;
    Ok((untracked, verbose, path))
}

#[test]
fn a_value_is_taken_only_when_attached() {
    let mode = |words: &[&str]| show(words).unwrap().0;
    assert_eq!(mode(&[]), None);
    assert_eq!(mode(&["-u"]).as_deref(), Some("all"));
    assert_eq!(mode(&["--untracked-files"]).as_deref(), Some("all"));
    assert_eq!(mode(&["-uno"]).as_deref(), Some("no"));
    assert_eq!(mode(&["-u=normal"]).as_deref(), Some("normal"));
    assert_eq!(mode(&["--untracked-files=no"]).as_deref(), Some("no"));
    // the next word is not its value
    assert_eq!(
        show(&["-u", "no"]).unwrap(),
        (Some("all".to_owned()), false, Some("no".to_owned()))
    );
    assert_eq!(
        show(&["--untracked-files", "-v", "file"]).unwrap(),
        (Some("all".to_owned()), true, Some("file".to_owned()))
    );
    // in a cluster the letters after it are its value
    assert_eq!(
        show(&["-vuno"]).unwrap(),
        (Some("no".to_owned()), true, None)
    );
}

#[test]
fn an_attached_value_is_still_checked_and_the_option_may_not_repeat() {
    let refused = show(&["-uevery"]).unwrap_err();
    assert!(refused.contains("invalid value 'every'"), "{refused}");
    assert!(show(&["--untracked-files=some"]).is_err());
    assert!(show(&["-u", "-uno"]).is_err());
}

#[test]
fn help_shows_the_value_as_optional() {
    let help = show(&["--help"]).unwrap_err();
    assert!(help.contains("-u, --untracked-files[=<MODE>]"), "{help}");
    assert!(help.contains("Usage: tool show [OPTIONS] [PATH]"), "{help}");
}
