//! Lines of a megabyte, as of a script written on one line: the hunk
//! headers are git's whichever driver reads them.

use std::path::Path;

use crate::funcname_drivers::{ok, pair, run, same_headers};

/// A driver, and a line it takes for a head when it stands alone.
const HEADS: [(&str, &str); 12] = [
    ("cpp", "static int run(struct thing *t) {"),
    ("java", "    public static void main(String[] args) {"),
    ("python", "def run(self):"),
    ("rust", "pub fn run(x: u32) -> u32 {"),
    ("golang", "func run(x int) int {"),
    ("php", "function run($x) {"),
    ("perl", "sub run {"),
    ("ruby", "def run"),
    ("css", ".run {"),
    ("markdown", "# run"),
    // what may match nothing, repeated without bound around groups
    ("loop", "ab ab"),
    ("default", "run"),
];

/// `head`, then statements up to a megabyte, all on one line.
fn long(head: &str) -> Vec<u8> {
    let mut line = head.as_bytes().to_vec();
    let mut number = 0;
    while line.len() < 1 << 20 {
        line.extend(format!(" var v{number}=f(a,b)||{{k:[1,2]}};if(x){{y()}}").bytes());
        number += 1;
    }
    line
}

/// A short head, the same head with a megabyte behind it, and a
/// megabyte that begins like no head; lines no driver takes for a head
/// between them, with `mark` in one behind each.
fn file(head: &str, mark: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for (at, line) in [head.as_bytes().to_vec(), long(head), long("  0;")]
        .iter()
        .enumerate()
    {
        out.extend_from_slice(line);
        out.push(b'\n');
        for below in 0..8 {
            let mark = if below == 5 { mark } else { "" };
            out.extend(format!("        0_{at}_{below}{mark}\n").bytes());
        }
    }
    out
}

#[test]
fn lines_of_a_megabyte_are_named_as_git_names_them() {
    let write = |root: &Path, mark: &str| {
        for (driver, head) in HEADS {
            std::fs::write(root.join(format!("long.{driver}")), file(head, mark)).unwrap();
        }
    };
    let prepare = |root: &Path| {
        write(root, "");
        let attributes: String = HEADS
            .iter()
            .filter(|(driver, _)| *driver != "default")
            .map(|(driver, _)| format!("*.{driver} diff={driver}\n"))
            .collect();
        std::fs::write(root.join(".gitattributes"), attributes).unwrap();
        let looped = "^((ab)*| *)*$|^((a?b?)*)";
        ok(run(
            "git",
            root,
            "C",
            &["config", "diff.loop.xfuncname", looped],
        ));
    };
    let dirs = pair(prepare, |root: &Path| write(root, " changed"));
    let headers = same_headers(&dirs, &["diff"], 3 * HEADS.len());
    // a head is named, also the one with a megabyte behind it
    let named = headers.iter().filter(|h| h.contains("run")).count();
    assert!(named >= 2 * 8 * crate::funcname_drivers::locales().len());
}
