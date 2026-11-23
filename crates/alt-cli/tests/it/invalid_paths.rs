//! A path one of whose steps a file system may take for the control
//! directory is refused by every command that would record it or write
//! there, as git refuses the like of `.git`.

use std::process::Command;

use crate::like_git::{Pair, alt, fails, index};

/// What stands in for the control directory: `{}` is `alt` or `git`.
const FORMS: [&str; 6] = [".{U}", ".{l}.", ".{l} ", "{U}~1", ".{l}\u{200c}", ".{U}:x"];

fn form(pattern: &str, name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    pattern.replace("{U}", &upper).replace("{l}", name)
}

#[test]
fn a_path_through_a_name_of_the_control_directory_is_refused() {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.commit_all("one");
    let staged = index(&pair.alt);
    for pattern in FORMS {
        let (ours, theirs) = (form(pattern, "alt"), form(pattern, "git"));
        std::fs::create_dir(pair.alt.join(&ours)).unwrap();
        std::fs::write(pair.alt.join(&ours).join("x"), "x\n").unwrap();
        std::fs::create_dir(pair.git.join(&theirs)).unwrap();
        std::fs::write(pair.git.join(&theirs).join("x"), "x\n").unwrap();
        // git, told to guard against both file systems as alt always does
        let by_git = Command::new("git")
            .current_dir(&pair.git)
            .args(["-c", "core.protectHFS=true", "-c", "core.protectNTFS=true"])
            .args(["add", &format!("{theirs}/x")])
            .output()
            .unwrap();
        assert!(!by_git.status.success(), "{theirs}");
        let said = String::from_utf8_lossy(&by_git.stderr).into_owned();
        assert!(
            said.contains(&format!("invalid path '{theirs}/x'")),
            "{said}"
        );

        let path = format!("{ours}/x");
        let said = fails(alt(&pair.alt, &["add", &path]), 128);
        assert!(
            said.starts_with(&format!("error: invalid path '{path}'\n")),
            "{said}"
        );
        let moved = format!("{ours}/moved");
        let said = fails(alt(&pair.alt, &["mv", "a", &moved]), 128);
        assert!(
            said.starts_with(&format!("error: invalid path '{moved}'\n")),
            "{said}"
        );
        assert!(pair.alt.join("a").exists() && !pair.alt.join(&moved).exists());
        assert_eq!(index(&pair.alt), staged, "{ours}");
        std::fs::remove_dir_all(pair.alt.join(&ours)).unwrap();
    }
}
