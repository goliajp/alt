//! The Myers diff against `git diff` itself: generated file pairs must
//! come out with the same hunks, line for line.

use std::path::Path;
use std::process::Command;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A line drawn from `variety` distinct texts. Some are blank, the rest
/// carry varying indentation, so that runs of changes have room to slide.
fn line(rng: &mut Rng, variety: usize) -> String {
    let k = rng.below(variety);
    match k % 7 {
        0 => String::new(),
        1 => "}".to_owned(),
        // unindented lines are what hunk headers quote
        2 => format!("fn section{k}() {{   "),
        _ => format!("{}stmt{k};", "    ".repeat(k % 4)),
    }
}

fn old_file(rng: &mut Rng, lines: usize, variety: usize) -> Vec<String> {
    (0..lines).map(|_| line(rng, variety)).collect()
}

/// Edits a copy of `old`: runs of lines removed, inserted, repeated or
/// moved, `edits` times.
fn new_file(rng: &mut Rng, old: &[String], edits: usize, variety: usize) -> Vec<String> {
    let mut new = old.to_vec();
    for _ in 0..edits {
        let at = rng.below(new.len() + 1);
        let run = 1 + rng.below(6);
        match rng.below(5) {
            0 => {
                let end = (at + run).min(new.len());
                new.drain(at.min(end)..end);
            }
            1 => {
                for i in 0..run {
                    new.insert(at + i, line(rng, variety));
                }
            }
            2 if !new.is_empty() => {
                // repeat a run found elsewhere in the file
                let from = rng.below(new.len());
                let copy: Vec<String> = new[from..(from + run).min(new.len())].to_vec();
                for (i, l) in copy.into_iter().enumerate() {
                    new.insert((at + i).min(new.len()), l);
                }
            }
            3 if !new.is_empty() => {
                let from = rng.below(new.len());
                let end = (from + run).min(new.len());
                let moved: Vec<String> = new.drain(from..end).collect();
                let to = rng.below(new.len() + 1);
                for (i, l) in moved.into_iter().enumerate() {
                    new.insert(to + i, l);
                }
            }
            _ => {
                if at < new.len() {
                    new[at] = line(rng, variety);
                }
            }
        }
    }
    new
}

fn join(lines: &[String], final_newline: bool) -> Vec<u8> {
    let mut text = lines.join("\n").into_bytes();
    if final_newline && !lines.is_empty() {
        text.push(b'\n');
    }
    text
}

/// git's hunks for the two files.
fn git_hunks(dir: &Path, old: &[u8], new: &[u8]) -> String {
    std::fs::write(dir.join("old"), old).unwrap();
    std::fs::write(dir.join("new"), new).unwrap();
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["diff", "--no-index", "--no-color", "old", "new"])
        .output()
        .unwrap();
    assert!(out.status.code().is_some_and(|c| c <= 1), "git diff failed");
    let text = String::from_utf8(out.stdout).unwrap();
    match text.find("\n@@") {
        Some(at) => text[at + 1..].to_owned(),
        None => String::new(),
    }
}

fn ours(old: &[u8], new: &[u8]) -> String {
    let mut out = Vec::new();
    alt_diff::write_unified(&mut out, old, new, 3);
    String::from_utf8(out).unwrap()
}

fn check(dir: &Path, seed: u64, lines: usize, variety: usize, edits: usize) {
    let mut rng = Rng(seed);
    let old = old_file(&mut rng, lines, variety);
    let new = new_file(&mut rng, &old, edits, variety);
    let (old, new) = (
        join(&old, !seed.is_multiple_of(3)),
        join(&new, !seed.is_multiple_of(5)),
    );
    assert_eq!(
        ours(&old, &new),
        git_hunks(dir, &old, &new),
        "seed {seed}, {lines} lines of {variety} kinds, {edits} edits"
    );
}

#[test]
fn small_files_with_many_repeated_lines_match_git() {
    let dir = alt_os::temp::tempdir().unwrap();
    for seed in 1..=400u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let lines = 1 + rng.below(60);
        let variety = 2 + rng.below(12);
        let edits = 1 + rng.below(8);
        check(dir.path(), seed, lines, variety, edits);
    }
}

#[test]
fn medium_files_match_git() {
    let dir = alt_os::temp::tempdir().unwrap();
    for seed in 1000..1150u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let lines = 100 + rng.below(900);
        let variety = 5 + rng.below(400);
        let edits = 1 + rng.below(60);
        check(dir.path(), seed, lines, variety, edits);
    }
}

#[test]
fn large_costly_files_match_git() {
    // enough lines and changes for the search to cut corners, and few
    // enough kinds of line for some to count as too common to anchor on
    let dir = alt_os::temp::tempdir().unwrap();
    for (seed, lines, variety, edits) in [
        (2001u64, 6000, 40, 900),
        (2002, 9000, 3000, 2500),
        (2003, 12000, 200, 1500),
        (2004, 5000, 5000, 3000),
        (2005, 20000, 15, 400),
        (2006, 8000, 800, 4000),
    ] {
        check(dir.path(), seed, lines, variety, edits);
    }
}

/// Nested blocks of statements, the shape of source code: inserting or
/// removing a block leaves a run that can slide over the braces and blank
/// lines around it.
fn code_file(rng: &mut Rng, blocks: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    for _ in 0..blocks {
        let pad = "\t".repeat(depth);
        match rng.below(6) {
            0 if depth < 4 => {
                out.push(format!("{pad}if (cond{}) {{", rng.below(3)));
                depth += 1;
            }
            1 if depth > 0 => {
                depth -= 1;
                out.push(format!("{}}}", "\t".repeat(depth)));
                if rng.below(2) == 0 {
                    out.push(String::new());
                }
            }
            2 => out.push(String::new()),
            3 => out.push(format!("{pad}return;")),
            _ => out.push(format!("{pad}call{}();", rng.below(4))),
        }
    }
    while depth > 0 {
        depth -= 1;
        out.push(format!("{}}}", "\t".repeat(depth)));
    }
    out
}

#[test]
fn code_shaped_files_match_git() {
    let dir = alt_os::temp::tempdir().unwrap();
    for seed in 3000..3300u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let blocks = 20 + rng.below(200);
        let old = code_file(&mut rng, blocks);
        // splice whole stretches of another such file in and out
        let donor = code_file(&mut rng, 150);
        let mut new = old.clone();
        for _ in 0..1 + rng.below(6) {
            let at = rng.below(new.len() + 1);
            if rng.below(2) == 0 {
                let from = rng.below(donor.len());
                let piece = &donor[from..(from + 1 + rng.below(12)).min(donor.len())];
                new.splice(at..at, piece.iter().cloned());
            } else {
                let end = (at + 1 + rng.below(12)).min(new.len());
                new.drain(at..end);
            }
        }
        let (old, new) = (join(&old, true), join(&new, true));
        assert_eq!(
            ours(&old, &new),
            git_hunks(dir.path(), &old, &new),
            "seed {seed}"
        );
    }
}

#[test]
fn reordered_stretches_between_long_common_runs_match_git() {
    // each stretch holds the same lines on both sides in a different
    // order, which is expensive to diff; the common run after it is long
    // enough for the search to stop early there
    let dir = alt_os::temp::tempdir().unwrap();
    for seed in 4000..4012u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let (mut old, mut new) = (Vec::new(), Vec::new());
        for stretch in 0..4 + rng.below(4) {
            let size = 300 + rng.below(500);
            let mut lines: Vec<String> =
                (0..size).map(|i| format!("s{stretch} item {i}")).collect();
            old.extend(lines.iter().cloned());
            for i in (1..lines.len()).rev() {
                lines.swap(i, rng.below(i + 1));
            }
            new.extend(lines);
            for i in 0..25 + rng.below(40) {
                let common = format!("s{stretch} common {i}");
                old.push(common.clone());
                new.push(common);
            }
        }
        let (old, new) = (join(&old, true), join(&new, true));
        assert_eq!(
            ours(&old, &new),
            git_hunks(dir.path(), &old, &new),
            "seed {seed}"
        );
    }
}

#[test]
fn long_files_with_scattered_changes_match_git() {
    // hundreds of small changes with unchanged runs of every length
    // between them: the search gets expensive while still moving fast
    let dir = alt_os::temp::tempdir().unwrap();
    for (seed, lines, variety, edits) in [
        (5001u64, 30000, 2000, 1500),
        (5002, 40000, 6000, 2600),
        (5003, 25000, 900, 1100),
    ] {
        check(dir.path(), seed, lines, variety, edits);
    }
}

#[test]
fn inserted_sections_with_common_separator_lines_match_git() {
    // blank and brace lines occur everywhere; inside a section of new
    // text they are not worth matching against the rest of the file
    let dir = alt_os::temp::tempdir().unwrap();
    for seed in 6000..6060u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut fresh = 0;
        let mut section = |rng: &mut Rng, len: usize| -> Vec<String> {
            (0..len)
                .map(|_| match rng.below(4) {
                    0 => String::new(),
                    1 => "}".to_owned(),
                    _ => {
                        fresh += 1;
                        format!("text {fresh}")
                    }
                })
                .collect()
        };
        let len = 400 + rng.below(1500);
        let old = section(&mut rng, len);
        let mut new = old.clone();
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(new.len() + 1);
            let len = 5 + rng.below(200);
            if rng.below(3) == 0 {
                new.drain(at..(at + len).min(new.len()));
            } else {
                let piece = section(&mut rng, len);
                new.splice(at..at, piece);
            }
        }
        let (old, new) = (join(&old, true), join(&new, true));
        assert_eq!(
            ours(&old, &new),
            git_hunks(dir.path(), &old, &new),
            "seed {seed}"
        );
    }
}

#[test]
fn one_side_empty_and_identical_files_match_git() {
    let dir = alt_os::temp::tempdir().unwrap();
    for (old, new) in [
        (&b""[..], &b"a\nb\n"[..]),
        (b"a\nb\n", b""),
        (b"a\nb\n", b"a\nb\n"),
        (b"a\nb", b"a\nb\n"),
        (b"a\nb\n", b"a\nb"),
        (b"\n\n\n", b"\n\n"),
    ] {
        assert_eq!(ours(old, new), git_hunks(dir.path(), old, new));
    }
}
