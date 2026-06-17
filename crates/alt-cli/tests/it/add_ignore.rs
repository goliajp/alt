//! `alt add .` honours `.gitignore`. The end-to-end smoke test the
//! self-hosting workflow blew up on: with no ignore handling, `alt add .`
//! used to swallow every gitignored path (caches, dev sandboxes, the
//! `.alt` store itself). This locks the fix in place.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn alt_add_dot_skips_gitignored_paths_at_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));

    // The exact shape `alt`'s own project root uses
    std::fs::write(root.join(".gitignore"), "/.dev/\n/target/\n*.log\n").unwrap();

    // tracked
    std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "// hi\n").unwrap();

    // ignored — these used to be staged by an unfiltered `alt add .`
    std::fs::create_dir(root.join(".dev")).unwrap();
    std::fs::write(root.join(".dev/heavy.bin"), [0xff; 65536]).unwrap();
    std::fs::create_dir(root.join("target")).unwrap();
    std::fs::write(root.join("target/build.out"), [0xab; 4096]).unwrap();
    std::fs::write(root.join("run.log"), "noise\n").unwrap();

    let stdout = ok(alt(root, &["add", "."]));

    // staged count covers the tracked entries only:
    //   .gitignore, Cargo.toml, src/lib.rs
    let staged: usize = stdout
        .split_whitespace()
        .find_map(|tok| tok.parse::<usize>().ok())
        .unwrap_or_else(|| panic!("could not parse staged count from {stdout:?}"));
    assert_eq!(staged, 3, "want 3 tracked entries staged, got {stdout}");

    // and a status / commit cycle proves the ignored content doesn't reach
    // the store — a fresh commit would otherwise refuse to start (huge
    // staged delete) or carry the ignored payload into the tree.
    ok(alt(root, &["commit", "-m", "first"]));
    let log = ok(alt(root, &["log", "-n", "1", "--json"]));
    assert!(log.contains("\"tree\":"), "no tree in commit: {log}");
}
