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

/// Whether `node` is fixed bytes and nothing else: where they lie in a
/// text is then where it matches.
pub fn alone(node: &Node) -> bool {
    let fixed = |node: &Node| matches!(node, Node::Set(set) if set.single().is_some());
    match node {
        Node::Concat(items) => items.iter().all(fixed),
        _ => fixed(node),
    }
}

/// Whether every match of `node` begins with the bytes `fixed`, as
/// far as the pattern says so in as many words: `fixed` is the run of
/// fixed bytes it begins with.
pub fn leads(node: &Node, fixed: &[u8]) -> bool {
    let mut run = Vec::new();
    lead(node, &mut run);
    run == fixed
}

/// Adds the fixed bytes `node` begins with to `run`; false when
/// something else may come behind them.
fn lead(node: &Node, run: &mut Vec<u8>) -> bool {
    match node {
        Node::Set(set) => set.single().map(|b| run.push(b)).is_some(),
        Node::Empty => true,
        Node::Group(inner, _) => lead(inner, run),
        Node::Concat(items) => items.iter().all(|item| lead(item, run)),
        _ => false,
    }
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
        Node::Alt(_) | Node::Repeat(..) | Node::BackRef(_) | Node::Class(_) => close(run, best),
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
            assert!(!alone(&node), "{pat}");
        }
        for (pat, fixed, led) in [
            ("foo(bar)baz+", "foobarba", true),
            ("abc(d|e)", "abc", true),
            ("ab(cd|ef)ghi", "ghi", false),
            ("x*abc", "abc", false),
            ("(ab)+c", "ab", false),
        ] {
            let (node, _) = parse(pat.as_bytes(), Options::default()).unwrap();
            assert_eq!(leads(&node, fixed.as_bytes()), led, "{pat}");
        }
        for pat in ["a", "static int", "a\\.b"] {
            let (node, _) = parse(pat.as_bytes(), Options::default()).unwrap();
            assert!(alone(&node), "{pat}");
        }
    }
}
