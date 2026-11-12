//! Accepted command lines read into the right fields.

use std::path::PathBuf;

use alt_args::Parser;

use crate::cli::{BisectOp, Cli, Command, WorkspaceOp};
use crate::server::{Action, Args, SecretKind};

fn alt(line: &str) -> Cli {
    Cli::try_parse_from(line.split(' ').map(String::from)).unwrap_or_else(|e| panic!("{e}"))
}

fn server(line: &str) -> Action {
    let words = line.split(' ').map(String::from);
    Args::try_parse_from(words)
        .unwrap_or_else(|e| panic!("{e}"))
        .command
}

#[test]
fn option_spellings() {
    let line = "alt commit -m msg --decision one --decision=two --meta k=v=w -n";
    let Command::Commit {
        message,
        amend,
        no_verify,
        decisions,
        extra,
        ..
    } = alt(line).command
    else {
        panic!()
    };
    assert_eq!(message.as_deref(), Some("msg"));
    assert!(no_verify && !amend);
    assert_eq!(decisions, ["one", "two"]);
    assert_eq!(extra, [("k".to_owned(), "v=w".to_owned())]);

    for line in [
        "alt log -pn3 main",
        "alt log -p -n=3 main",
        "alt log main -n 3 --patch",
    ] {
        let Command::Log(args) = alt(line).command else {
            panic!()
        };
        assert!(args.patch && !args.json, "{line}");
        assert_eq!((args.max_count, args.rev.as_str()), (Some(3), "main"));
        assert_eq!(args.pretty, "oneline");
    }
    let Command::Commit { message, .. } = alt("alt commit -m -").command else {
        panic!()
    };
    assert_eq!(message.as_deref(), Some("-"));
}

#[test]
fn double_dash_ends_options() {
    let Command::Add { paths, json } = alt("alt add a -- -x --json").command else {
        panic!()
    };
    assert_eq!(paths, ["a", "-x", "--json"]);
    assert!(!json);
}

#[test]
fn trailing_words_belong_to_the_command() {
    for (line, expected) in [
        (
            "alt bisect run sh -c exit --json -x",
            "sh -c exit --json -x",
        ),
        ("alt bisect run -- make test", "make test"),
        ("alt bisect run make -- test", "make -- test"),
        ("alt bisect run --unknown -x", "--unknown -x"),
        ("alt bisect run x --help", "x --help"),
    ] {
        let Command::Bisect {
            op: BisectOp::Run { cmd },
        } = alt(line).command
        else {
            panic!()
        };
        assert_eq!(cmd.join(" "), expected);
    }
}

#[test]
fn global_options_are_accepted_at_every_level() {
    let cli = alt("alt --workspace a status --workspace b");
    assert_eq!(cli.workspace.as_deref(), Some("b"));
    let cli = alt("alt status --request-id=00ff");
    assert_eq!(cli.request_id.as_deref(), Some("00ff"));
    assert_eq!(cli.workspace, None);
    for line in [
        "alt workspace --json add t /tmp/t",
        "alt workspace add t /tmp/t --json",
        "alt workspace add --json t /tmp/t",
    ] {
        let Command::Workspace { op, json } = alt(line).command else {
            panic!()
        };
        assert!(json, "{line}");
        let WorkspaceOp::Add { name, path, branch } = op else {
            panic!()
        };
        assert_eq!(
            (name.as_str(), path, branch),
            ("t", PathBuf::from("/tmp/t"), None)
        );
    }
}

#[test]
fn typed_values_and_defaults() {
    let Action::Secret { kind, output } = server("alt-server secret receipt-key --output o") else {
        panic!()
    };
    assert_eq!((kind, output), (SecretKind::ReceiptKey, PathBuf::from("o")));
    let Action::ReviewGrant { allow, .. } = server("alt-server review-grant r a --allow false")
    else {
        panic!()
    };
    assert!(!allow);
    let Action::Backup { schema, .. } = server("alt-server backup --recipient r --output o") else {
        panic!()
    };
    assert_eq!(schema, crate::server::SCHEMA_VERSION);
    let line = "alt-server cloud-initialize --spec s --directory d --assets a";
    let Action::CloudInitialize {
        listen,
        database_environment,
        ..
    } = server(line)
    else {
        panic!()
    };
    assert_eq!(listen, "127.0.0.1:8081".parse().unwrap());
    assert_eq!(database_environment, "ALT_SERVER_DATABASE");
    let Action::Audit { before, limit } = server("alt-server audit --before 7") else {
        panic!()
    };
    assert_eq!((before, limit), (Some(7), 50));
}

#[test]
fn usage_names_the_program_as_it_was_called() {
    let words = ["/opt/bin/alt-next", "status", "extra"].map(String::from);
    let error = Cli::try_parse_from(words).err().unwrap();
    assert!(
        error
            .to_string()
            .contains("\nUsage: alt-next status [OPTIONS]\n")
    );
    let error = Cli::try_parse_from(["/opt/bin/alt-next", "--version"].map(String::from));
    assert_eq!(error.err().unwrap().to_string(), "alt 0.1.0\n");
}

/// What each level recorded, as `id=value,value` in order.
fn recorded(line: &str) -> Vec<String> {
    let words = line.split(' ').map(String::from);
    let matches = alt_args::parse(Cli::COMMAND, words).unwrap_or_else(|e| panic!("{e}"));
    let mut level = Some(&matches);
    let mut lines = Vec::new();
    while let Some(matches) = level {
        let entries = matches.entries().iter();
        let entries = entries.map(|entry| format!("{}={}", entry.id, entry.values.join(",")));
        lines.push(format!(
            "{}: {}",
            matches.name(),
            entries.collect::<Vec<_>>().join(" ")
        ));
        level = matches.subcommand();
    }
    lines
}

#[test]
fn levels_record_arguments_groups_and_defaults() {
    assert_eq!(
        recorded("alt cat-file -t abc"),
        [
            "alt: ",
            "cat-file: show_type=true CatFileArgs=show_type,object op=show_type object=abc \
             show_size=false pretty=false"
        ]
    );
    assert_eq!(
        recorded("alt --workspace w workspace --json add t /tmp/t"),
        [
            "alt: workspace=w Cli=workspace json=true",
            "workspace: json=true Workspace=json workspace=w",
            "add: name=t Add=name,path path=/tmp/t json=true workspace=w"
        ]
    );
    assert_eq!(
        recorded("alt portable export-status --credential-file c --journal=j"),
        [
            "alt: json=false",
            "portable: json=false",
            "export-status: credential_file=c PortableOptions=credential_file,journal journal=j \
             max_metadata_bytes=16777216 max_entries=1000000 max_publications=100000 \
             max_blob_bytes=8589934592 max_total_bytes=68719476736 timeout_seconds=120 json=false"
        ]
    );
}
