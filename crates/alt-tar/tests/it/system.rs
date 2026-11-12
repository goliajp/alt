use crate::{Members, read_files, write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "alt-tar-it-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir(&path).unwrap();
        Scratch(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn tree() -> Members {
    let names = [
        "plain".to_owned(),
        format!("{}/{}", "d".repeat(60), "m".repeat(90)),
        format!("{}/{}", "l".repeat(120), "n".repeat(200)),
        format!("unicode/{}", "\u{540d}".repeat(40)),
    ];
    names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let data = (0..index * 70_000).map(|i| (i * 13) as u8).collect();
            (name.into_bytes(), data)
        })
        .collect()
}

fn populate(root: &Path, members: &Members) {
    for (path, data) in members {
        let path = root.join(std::str::from_utf8(path).unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
}

fn run(program: &str, directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new(program)
        .current_dir(directory)
        .env("COPYFILE_DISABLE", "1")
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program} {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// `tar` always, plus whichever of the other implementation's names exist
fn programs() -> Vec<&'static str> {
    let mut found = vec!["tar"];
    for program in ["bsdtar", "gtar"] {
        if Command::new(program).arg("--version").output().is_ok() {
            found.push(program);
        }
    }
    found
}

fn is_gnu(program: &str) -> bool {
    let version = run(program, Path::new("."), &["--version"]);
    String::from_utf8_lossy(&version).contains("GNU tar")
}

#[test]
fn system_tar_reads_what_the_builder_writes() {
    let members = tree();
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("archive.tar"), write(&members)).unwrap();
    for program in programs() {
        let listing = run(program, &scratch.0, &["-tf", "archive.tar"]);
        let listed: Vec<&[u8]> = listing.split(|&b| b == b'\n').collect();
        for (path, _) in &members {
            assert!(
                listed.contains(&&path[..]),
                "{program} did not list a member"
            );
        }
        let out = scratch.0.join(program);
        std::fs::create_dir(&out).unwrap();
        run(program, &out, &["-xf", "../archive.tar"]);
        for (path, data) in &members {
            let extracted = std::fs::read(out.join(std::str::from_utf8(path).unwrap()));
            assert!(
                &extracted.unwrap() == data,
                "{program} extracted other bytes"
            );
        }
    }
}

#[test]
fn archive_reads_what_system_tar_writes() {
    let members = tree();
    let scratch = Scratch::new();
    let source = scratch.0.join("source");
    populate(&source, &members);
    let names: Vec<&str> = members
        .iter()
        .map(|(path, _)| std::str::from_utf8(path).unwrap())
        .collect();
    for program in programs() {
        let gnu = if is_gnu(program) { "gnu" } else { "gnutar" };
        for format in [None, Some("pax"), Some(gnu)] {
            let mut arguments = vec!["-cf", "-"];
            let flag = format.map(|format| format!("--format={format}"));
            arguments.extend(flag.as_deref());
            arguments.extend(&names);
            let archive = run(program, &source, &arguments);
            assert!(
                read_files(&archive).unwrap() == members,
                "{program} {format:?}"
            );
        }
        let archive = run(
            program,
            &source,
            &["--format=ustar", "-cf", "-", names[0], names[1]],
        );
        assert!(
            read_files(&archive).unwrap() == members[..2],
            "{program} ustar"
        );
        let archive = run(program, &source, &["-cf", "-", "."]);
        let mut found = read_files(&archive).unwrap();
        for (path, _) in &mut found {
            assert!(path.starts_with(b"./"));
            path.drain(..2);
        }
        found.sort();
        let mut expected = members.clone();
        expected.sort();
        assert!(found == expected, "{program} whole directory");
    }
}
