//! Every recorded command line gives the recorded outcome. The records
//! were taken from the same commands built on clap 4.6, which this crate
//! replaced: help and version on standard output with status 0, a rejected
//! command line on standard error with status 2, and an accepted one as
//! status 0 with no output.

use alt_args::{ErrorKind, Parser};

struct Record<'a> {
    args: &'a str,
    status: &'a str,
    stdout: &'a str,
    stderr: &'a str,
}

fn records(fixture: &str) -> Vec<Record<'_>> {
    let fixture = fixture.strip_prefix("### ").unwrap();
    let mut records = Vec::new();
    for text in fixture.split("\n### ") {
        let (args, rest) = text.split_once('\n').unwrap();
        let (status, rest) = rest.split_once("\n--- stdout\n").unwrap();
        let (stdout, stderr) = rest.split_once("--- stderr\n").unwrap();
        records.push(Record {
            args,
            status,
            stdout,
            stderr,
        });
    }
    records
}

/// `wrap` turns an error message into what the program wrote for it.
fn check<P: Parser>(program: &str, fixture: &str, wrap: fn(&str) -> String) -> usize {
    let records = records(fixture);
    for record in &records {
        let words = std::iter::once(program).chain(record.args.split(' '));
        let words = words.filter(|word| !word.is_empty()).map(String::from);
        match (P::try_parse_from(words), record.status) {
            (Ok(_), "exit 0") => assert_eq!(record.stdout, "", "{}", record.args),
            (Err(error), "exit 0") => {
                assert_ne!(error.kind, ErrorKind::Usage, "{}", record.args);
                assert_eq!(error.to_string(), record.stdout, "{}", record.args);
            }
            (Err(error), "exit 2") => {
                assert_eq!(error.kind, ErrorKind::Usage, "{}", record.args);
                assert_eq!(wrap(&error.to_string()), record.stderr, "{}", record.args);
            }
            (Ok(_), _) => panic!("{}: accepted, recorded {}", record.args, record.status),
            (Err(error), _) => panic!("{}: {error}", record.args),
        }
    }
    records.len()
}

#[test]
fn version_control_command_line() {
    let fixture = include_str!("fixtures/alt.txt");
    let checked = check::<crate::cli::Cli>("alt", fixture, |message| format!("fatal: {message}\n"));
    assert_eq!(checked, 161);
}

#[test]
fn operator_command_line() {
    let fixture = include_str!("fixtures/server.txt");
    let checked = check::<crate::server::Args>("alt-server", fixture, str::to_owned);
    assert_eq!(checked, 57);
}
