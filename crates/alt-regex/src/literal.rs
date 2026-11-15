//! Bytes every match must contain, found before any automaton runs.

use crate::syntax::Node;

/// The longest run of fixed bytes that every match of `node` contains.
pub fn required(node: &Node) -> Option<Vec<u8>> {
    let mut best = Vec::new();
    let mut run = Vec::new();
    walk(node, &mut run, &mut best);
    close(&mut run, &mut best);
    (!best.is_empty()).then_some(best)
}

fn close(run: &mut Vec<u8>, best: &mut Vec<u8>) {
    if run.len() > best.len() {
        std::mem::swap(run, best);
    }
    run.clear();
}

fn walk(node: &Node, run: &mut Vec<u8>, best: &mut Vec<u8>) {
    match node {
        Node::Set(set) => match set.single() {
            Some(b) => run.push(b),
            None => close(run, best),
        },
        // these consume nothing: the bytes around them stay adjacent
        Node::Empty | Node::Look(_) => {}
        Node::Group(inner, _) => walk(inner, run, best),
        Node::Concat(items) => items.iter().for_each(|item| walk(item, run, best)),
        Node::Repeat(inner, min, _) if *min > 0 => {
            close(run, best);
            walk(inner, run, best);
            close(run, best);
        }
        Node::Alt(_) | Node::Repeat(..) | Node::BackRef(_) => close(run, best),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;
    use crate::syntax::parse;

    #[test]
    fn the_longest_fixed_run_outside_choices_is_required() {
        for (pat, literal) in [
            ("^[ \t]*(class|struct) x", Some(" x")),
            ("foo(bar)baz+", Some("foobarba")),
            ("a*b?", None),
            ("(ab)+c", Some("ab")),
            ("x|y", None),
            ("^static$", Some("static")),
            ("ab(cd|ef)ghi", Some("ghi")),
        ] {
            let (node, _) = parse(pat.as_bytes(), Options::default()).unwrap();
            let found = required(&node);
            assert_eq!(found.as_deref(), literal.map(str::as_bytes), "{pat}");
        }
    }
}
