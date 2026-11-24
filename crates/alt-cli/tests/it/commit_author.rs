//! `alt commit --author`: the name and the address are written as any
//! name and address are.

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
    for (author, said) in [(
        ",; <a@b>",
        "fatal: empty ident name (for <a@b>) not allowed\n",
    )] {
        let named = format!("--author={author}");
        let args = ["commit", "--allow-empty", "-m", "x", named.as_str()];
        of_late::same_failure(&pair, &args, 128, said);
    }
}
