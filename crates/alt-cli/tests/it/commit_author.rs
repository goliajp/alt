//! `alt commit --author`: the name and the address are written as any
//! name and address are, and a text that is no `Name <email>` names the
//! author git's `log --all` finds by it.

use std::process::Command;

use crate::like_git::{Pair, alt, git, ok};
use crate::of_late;

fn same_head(pair: &Pair, when: &str) -> String {
    let ours = ok(alt(&pair.alt, &["cat-file", "-p", "HEAD"]));
    assert_eq!(
        ours,
        ok(git(&pair.git, &["cat-file", "-p", "HEAD"])),
        "{when}"
    );
    ours
}

/// Commits `file` in both, by `author` and at `time`.
fn commit_by(pair: &Pair, file: &str, author: &str, time: u64) {
    pair.write(file, author);
    pair.both(&["add", file]);
    let date = format!("{time} +0000");
    let author = format!("--author={author}");
    let mut ours = crate::alt_command();
    ours.env("ALT_NO_DAEMON", "1").current_dir(&pair.alt);
    let mut theirs = Command::new("git");
    theirs
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(&pair.git);
    for mut command in [ours, theirs] {
        command
            .env("GIT_COMMITTER_NAME", "tester")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_DATE", &date)
            .args(["commit", "-m", file, &author]);
        ok(command.output().unwrap());
    }
}

#[test]
fn the_author_named_is_written_as_any_name_and_address_are() {
    let pair = Pair::new();
    pair.write("f", "0\n");
    pair.commit_all("one");
    for author in [
        "A\ncommitter evil <e@x> 1 +0000",
        " ,;Spaced Name;, < mail@x >",
        "Na\"me' <a<b@x>",
        "N\n\nM <e\n@x>",
        "Name <>",
    ] {
        pair.write("f", author);
        pair.both(&["add", "f"]);
        let named = format!("--author={author}");
        pair.both(&["commit", "-m", "by", &named]);
        let raw = same_head(&pair, author);
        // a tree, a parent, an author and a committer, and no line more
        let head: Vec<&str> = raw.lines().take_while(|l| !l.is_empty()).collect();
        assert_eq!(head.len(), 4, "{raw}");
        assert!(head[3].starts_with("committer tester <t@e> "), "{raw}");
    }
    for (author, said) in [
        ("abc>", "fatal: malformed --author parameter\n"),
        // with a `>` in it a text is no pattern
        ("t.*@e>$", "fatal: malformed --author parameter\n"),
        (
            ",; <a@b>",
            "fatal: empty ident name (for <a@b>) not allowed\n",
        ),
    ] {
        let named = format!("--author={author}");
        let args = ["commit", "--allow-empty", "-m", "x", named.as_str()];
        of_late::same_failure(&pair, &args, 128, said);
    }
}

#[test]
fn a_text_names_the_author_git_finds_by_it_among_all_refs() {
    let pair = Pair::new();
    commit_by(&pair, "a", "Alice Old <alice@old>", 1_700_000_100);
    pair.both(&["switch", "-c", "side"]);
    commit_by(&pair, "b", "Bob Side <bob@side>", 1_700_000_300);
    commit_by(&pair, "t", "Tie Side <tie@side>", 1_700_000_400);
    pair.both(&["switch", "main"]);
    commit_by(&pair, "c", "Alice New <alice@new>", 1_700_000_200);
    commit_by(&pair, "u", "Tie Main <tie@main>", 1_700_000_400);
    pair.both(&["switch", "-c", "gone"]);
    commit_by(&pair, "d", "Dora Tagged <dora@tag>", 1_700_000_500);
    pair.both(&["tag", "kept"]);
    pair.both(&["switch", "main"]);
    pair.both(&["branch", "-f", "gone", "main"]);
    for (text, by) in [
        // on a branch that is not checked out, and under a tag alone
        ("bob", "Bob Side <bob@side>"),
        ("dora", "Dora Tagged <dora@tag>"),
        // the commit made last, whatever branch has it
        ("alice", "Alice New <alice@new>"),
        // a pattern, not the letters of it
        ("^alice o", "Alice Old <alice@old>"),
        ("a.*@old.$", "Alice Old <alice@old>"),
        ("b.b s", "Bob Side <bob@side>"),
        ("[d-e]ora", "Dora Tagged <dora@tag>"),
        // two made at one moment
        ("tie", "Tie Main <tie@main>"),
    ] {
        let named = format!("--author={text}");
        pair.both(&["commit", "--allow-empty", "-m", "found", &named]);
        let raw = same_head(&pair, text);
        assert!(raw.contains(&format!("\nauthor {by} ")), "{text}: {raw}");
    }
    for text in ["a.c", "alice$", "x\\{2\\}"] {
        let named = format!("--author={text}");
        let said = format!(
            "fatal: --author '{text}' is not 'Name <email>' and matches no existing author\n"
        );
        let args = ["commit", "--allow-empty", "-m", "x", named.as_str()];
        of_late::same_failure(&pair, &args, 128, &said);
    }
}
