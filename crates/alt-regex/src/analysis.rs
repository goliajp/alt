//! What a matcher needs to know about the instructions before it runs.

use crate::class::ByteSet;
use crate::program::{Inst, NO_JOIN};
use crate::syntax::{Look, Node};

/// Whether a match can end both ways: behind a condition with nothing
/// consumed after it, and without one. The C library keeps the second
/// kind of path when it has both, whichever the pattern prefers.
pub fn mixed_ends(insts: &[Inst], start: u32, exits: &[u32]) -> bool {
    const PLAIN: u8 = 1;
    const BEHIND: u8 = 2;
    // how the match is reached from an instruction with nothing
    // consumed; for one that consumes, how from behind it, shifted
    let reached = |reach: &[u8], inst: &Inst| match *inst {
        Inst::Match => PLAIN,
        Inst::Save(_, next) | Inst::SaveOpt(_, next) => reach[next as usize] & 3,
        Inst::Split(first, second) => (reach[first as usize] | reach[second as usize]) & 3,
        Inst::Look(_, next) if reach[next as usize] & 3 != 0 => BEHIND,
        Inst::Look(..) => 0,
        Inst::Set(_, next) | Inst::Byte(_, next) | Inst::BackRef(_, _, next) => {
            (reach[next as usize] & 3) << 2
        }
        // some place of the set is its last
        Inst::Switch(copied, _) => (reach[exits[copied as usize] as usize] & 3) << 2,
    };
    let mut reach = vec![0u8; insts.len()];
    // an instruction is made after the ones it leads to, but for the
    // few that close a loop: one turn settles all the others
    let mut late = Vec::new();
    for (pc, inst) in insts.iter().enumerate() {
        let ahead = match *inst {
            Inst::Split(first, second) => first.max(second) as usize > pc,
            Inst::Save(_, next)
            | Inst::SaveOpt(_, next)
            | Inst::Look(_, next)
            | Inst::Set(_, next)
            | Inst::Byte(_, next)
            | Inst::BackRef(_, _, next) => next as usize > pc,
            Inst::Match | Inst::Switch(..) => false,
        };
        if ahead {
            late.push(pc);
        }
        reach[pc] = reached(&reach, inst);
    }
    while late
        .iter()
        .any(|&pc| reach[pc] != reached(&reach, &insts[pc]))
    {
        for (pc, inst) in insts.iter().enumerate() {
            reach[pc] |= reached(&reach, inst);
        }
    }
    let ends = reach.iter().fold(0, |all, &how| all | how >> 2) | reach[start as usize];
    ends & PLAIN != 0 && ends & BEHIND != 0
}

/// Numbers the instructions that more than one path leads into. Behind
/// each set of characters copied in comes one of `exits`. Inside a set
/// the byte read decides the path, so its places are no joins.
pub fn joins(insts: &[Inst], start: u32, exits: &[u32]) -> (Vec<u32>, u32) {
    let mut ways = vec![0u32; insts.len()];
    ways[start as usize] = 1;
    for inst in insts {
        match *inst {
            Inst::Set(_, next)
            | Inst::Byte(_, next)
            | Inst::Save(_, next)
            | Inst::SaveOpt(_, next)
            | Inst::BackRef(_, _, next)
            | Inst::Look(_, next) => ways[next as usize] += 1,
            Inst::Split(first, second) => {
                ways[first as usize] += 1;
                ways[second as usize] += 1;
            }
            Inst::Match | Inst::Switch(..) => {}
        }
    }
    for &exit in exits {
        ways[exit as usize] += 1;
    }
    let mut count = 0;
    let numbered = |ways: u32| match ways > 1 {
        true => {
            count += 1;
            count - 1
        }
        false => NO_JOIN,
    };
    (ways.into_iter().map(numbered).collect(), count)
}

/// How many instructions a path waits at, and how many leave a choice
/// or a slot to come back to.
pub fn counts(insts: &[Inst]) -> (u32, u32) {
    let (mut wait, mut leave) = (0, 0);
    for inst in insts {
        match inst {
            Inst::Set(..) | Inst::Byte(..) | Inst::Switch(..) | Inst::Match => wait += 1,
            Inst::Split(..) | Inst::Save(..) | Inst::SaveOpt(..) => leave += 1,
            Inst::Look(..) | Inst::BackRef(..) => {}
        }
    }
    (wait, leave)
}

/// Whether nothing can match before `node` except at the start of the text.
pub fn anchored(node: &Node) -> bool {
    match node {
        Node::Look(Look::TextStart) => true,
        Node::Group(inner, _) => anchored(inner),
        Node::Concat(items) => items.first().is_some_and(anchored),
        Node::Alt(branches) => branches.iter().all(anchored),
        Node::Repeat(inner, min, _) => *min > 0 && anchored(inner),
        _ => false,
    }
}

pub fn classes(insts: &[Inst], sets: &[ByteSet]) -> ([u8; 256], Vec<u8>) {
    let mut singles = ByteSet::empty();
    for inst in insts {
        if let Inst::Byte(b, _) = inst {
            singles.insert(*b);
        }
    }
    // the bytes are told apart by one set after another; a byte alone
    // in its part is set aside, since nothing splits it further
    let mut parts = vec![ByteSet::full()];
    let mut alone = Vec::new();
    for set in sets {
        for at in (0..parts.len()).rev() {
            let part = parts[at];
            let mut inside = part;
            inside.intersect(set);
            if inside.is_empty() || inside == part {
                continue;
            }
            let mut outside = part;
            outside.subtract(&inside);
            parts.swap_remove(at);
            for half in [inside, outside] {
                match half.len() {
                    1 => alone.push(half),
                    _ => parts.push(half),
                }
            }
        }
    }
    parts.extend(alone);
    let count = parts.len();
    let mut classes = [0u8; 256];
    for (number, part) in parts.iter().enumerate() {
        part.each(|b| classes[usize::from(b)] = number as u8);
    }
    // numbered as they come, a byte matched on its own in a class of
    // its own
    let mut numbers = vec![None; count];
    let mut members = Vec::new();
    for b in 0..=255u8 {
        let own = singles.contains(b);
        let known = numbers[usize::from(classes[usize::from(b)])].filter(|_| !own);
        let class = known.unwrap_or_else(|| {
            members.push(b);
            members.len() - 1
        });
        if !own {
            numbers[usize::from(classes[usize::from(b)])] = Some(class);
        }
        classes[usize::from(b)] = u8::try_from(class).expect("at most 256 classes");
    }
    (classes, members)
}

#[cfg(test)]
mod tests {
    use crate::program::compile;
    use crate::syntax::parse_shaped;
    use crate::{Options, Syntax};

    fn program(pattern: &str, utf8: bool) -> crate::program::Program {
        let options = Options {
            utf8,
            syntax: Syntax::Extended,
            ..Options::default()
        };
        let (node, groups, shape) = parse_shaped(pattern.as_bytes(), options).unwrap();
        compile(&node, groups, options, shape).unwrap()
    }

    #[test]
    fn a_match_ends_both_ways_only_where_a_path_ends_behind_a_condition() {
        for (pattern, mixed) in [
            ("(a)$|a", true),
            ("(^)|x*", true),
            ("x(^)|x()", true),
            ("(\\b|a)*b|c\\>", true),
            ("a$", false),
            ("^a", false),
            ("^(a*)|b", true),
            ("^a+|b", false),
            ("ab|c", false),
        ] {
            assert_eq!(program(pattern, false).mixed_ends, mixed, "{pattern}");
        }
    }

    #[test]
    fn the_instructions_inside_a_set_of_characters_are_no_joins() {
        let bytes = program("^([a-z]|x)+$", true);
        let characters = program("^([[:alpha:]]|x)+$", true);
        assert!(characters.insts.len() > 5 * bytes.insts.len());
        assert_eq!(characters.join_count, bytes.join_count);
    }
}
