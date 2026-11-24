//! The message of the tag a finish or `alt tag` makes, beside git's.

use crate::flow_tags::{finish_in_git, started, tags};
use crate::like_git::{alt, ok};

/// Git takes the lines that begin with `#`, the space at the end of a
/// line and the blank lines that say nothing out of the message of a tag.
#[test]
fn the_message_of_a_tag_is_cleaned_up_as_git_cleans_it() {
    const SAID: &str = "\n\nline one  \n\n\n# a comment\nbody\t\n\n  # indented\n#\n\n";
    let pair = started("release");
    for root in [&pair.alt, &pair.git] {
        std::fs::write(root.join("said"), SAID).unwrap();
    }
    let finish = ["flow", "release", "finish", "--messagefile", "said", "1.0"];
    ok(alt(&pair.alt, &finish));
    finish_in_git(&pair, "release", Some(&["-F", "said", "1.0"]));
    assert_eq!(tags(&pair.alt), tags(&pair.git));
    let pair = started("hotfix");
    ok(alt(
        &pair.alt,
        &["flow", "hotfix", "finish", "-m", SAID, "1.0"],
    ));
    finish_in_git(&pair, "hotfix", Some(&["-m", SAID, "1.0"]));
    assert_eq!(tags(&pair.alt), tags(&pair.git));
    // and the tag a command makes by itself: with nothing left of the
    // message, it has none
    for (name, said) in [("cleaned", SAID), ("nothing", "# only\n")] {
        pair.both(&["tag", "-a", "-m", said, name]);
        assert_eq!(tags(&pair.alt), tags(&pair.git), "{name}");
    }
}
