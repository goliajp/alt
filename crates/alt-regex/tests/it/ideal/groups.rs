//! Where the groups of a match lie by the rules of POSIX, found by
//! trying everything.
//!
//! The rules, from the definition of matching (XBD 9.1.2) and from the
//! description of `regexec` (XSH):
//!
//! 1. The match is the earliest, and of those the longest (XBD 9.1.2).
//! 2. "Consistent with the whole match being the longest of the leftmost
//!    matches, each subpattern, from left to right, shall match the
//!    longest possible string" (XBD 9.1.2). Of the things written one
//!    behind another, the first takes the most that leaves the rest a
//!    match, then the second, and so on.
//! 3. A repetition matches what its operand written again and again
//!    would (XBD 9.4.6), so its turns are subpatterns from left to
//!    right: the first turn takes the most that leaves the rest a match,
//!    then the second.
//! 4. A group that took part several times reports the last time
//!    (`regexec`, the first rule for `pmatch`).
//! 5. A group that took no part reports nothing: behind `*`, `?` or an
//!    interval when it matched zero times, or in the branch of an
//!    alternation that was not the one taken (the second rule).
//! 6. A group inside another reports by rules 4 and 5 within what the
//!    outer one reports: a group that took part only in an earlier turn
//!    of the group around it reports nothing (the third and fourth
//!    rules).
//! 7. A group that matched nothing reports the position it did so at
//!    (the fifth rule).
//!
//! What the standard leaves open, and is taken here either way:
//!
//! - which branch of an alternation is taken when more than one matches
//!   the same string;
//! - whether a repetition that has had its fewest turns takes one more
//!   turn of nothing where its last turn ended. "A null string shall be
//!   considered to be longer than no match at all" speaks for the turn,
//!   and no implementation takes it throughout.
//!
//! The C library does not keep rule 6: its groups are registers that a
//! later turn leaves as they were. This crate follows it there, so the
//! groups are found both ways: [`Kept::Last`] by rule 6, and
//! [`Kept::Stale`] with each group as an earlier turn left it.

use std::collections::BTreeSet;

use super::{Node, ends, ideal, step};

pub type Span = Option<(usize, usize)>;
/// For each group, from 0, where it lies.
pub type Report = Vec<Span>;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kept {
    Last,
    Stale,
}

struct Search<'a> {
    text: &'a [u8],
    groups: usize,
    kept: Kept,
}

/// The groups inside `node`.
fn inside(node: &Node, found: &mut Vec<usize>) {
    match node {
        Node::Set(_) | Node::Look(_) => {}
        Node::Group(inner, number) => {
            found.push(*number);
            inside(inner, found);
        }
        Node::Concat(items) | Node::Alt(items) => items.iter().for_each(|i| inside(i, found)),
        Node::Repeat(inner, ..) => inside(inner, found),
    }
}

/// Every pairing of a report of `first` with one of `then`, the second
/// laid over the first.
fn paired(first: &BTreeSet<Report>, then: &BTreeSet<Report>) -> BTreeSet<Report> {
    let over = |under: &Report, over: &Report| -> Report {
        under.iter().zip(over).map(|(u, o)| o.or(*u)).collect()
    };
    first
        .iter()
        .flat_map(|a| then.iter().map(move |b| over(a, b)))
        .collect()
}

impl Search<'_> {
    fn none(&self) -> BTreeSet<Report> {
        BTreeSet::from([vec![None; self.groups + 1]])
    }

    fn reaches(&self, items: &[Node], from: usize, to: usize) -> bool {
        let start = BTreeSet::from([from]);
        let ends = items.iter().fold(start, |now, i| step(i, self.text, &now));
        ends.contains(&to)
    }

    /// The reports of `node` matching exactly `from..to`, which it does.
    fn reports(&self, node: &Node, from: usize, to: usize) -> BTreeSet<Report> {
        match node {
            Node::Set(_) | Node::Look(_) => self.none(),
            Node::Group(inner, number) => {
                let mut own = vec![None; self.groups + 1];
                own[*number] = Some((from, to));
                paired(&self.reports(inner, from, to), &BTreeSet::from([own]))
            }
            Node::Concat(items) => self.sequence(items, from, to),
            Node::Alt(branches) => branches
                .iter()
                .filter(|branch| ends(branch, self.text, from).contains(&to))
                .flat_map(|branch| self.reports(branch, from, to))
                .collect(),
            Node::Repeat(inner, min, max) => {
                let turns = Turns {
                    inner,
                    min: *min,
                    max: *max,
                    to,
                };
                self.turns(&turns, from, 0)
            }
        }
    }

    fn sequence(&self, items: &[Node], from: usize, to: usize) -> BTreeSet<Report> {
        let Some((first, rest)) = items.split_first() else {
            return self.none();
        };
        let reached = ends(first, self.text, from);
        let fits = |end: &&usize| self.reaches(rest, **end, to);
        let end = *reached.iter().rev().find(fits).expect("a match");
        paired(
            &self.reports(first, from, end),
            &self.sequence(rest, end, to),
        )
    }

    /// Whether turns from `at`, behind `done` of them, can end at the
    /// end of the repetition.
    fn completes(&self, turns: &Turns, at: usize, done: u32) -> bool {
        if at == turns.to && done >= turns.min {
            return true;
        }
        if turns.max.is_some_and(|max| done >= max) {
            return false;
        }
        ends(turns.inner, self.text, at)
            .into_iter()
            .filter(|&end| end > at || done < turns.min)
            .any(|end| self.completes(turns, end, done + 1))
    }

    /// One turn from `at` to `end` and what follows it.
    fn turn(
        &self,
        turns: &Turns,
        at: usize,
        end: usize,
        then: &BTreeSet<Report>,
    ) -> BTreeSet<Report> {
        let mut now = self.reports(turns.inner, at, end);
        if self.kept == Kept::Stale {
            return paired(&now, then);
        }
        // a later turn leaves nothing of the groups of this one
        let mut within = Vec::new();
        inside(turns.inner, &mut within);
        let later: BTreeSet<Report> = then
            .iter()
            .filter(|report| within.iter().any(|&group| report[group].is_some()))
            .cloned()
            .collect();
        let last: BTreeSet<Report> = then.difference(&later).cloned().collect();
        now = paired(&now, &last);
        now.extend(later);
        now
    }

    fn turns(&self, turns: &Turns, at: usize, done: u32) -> BTreeSet<Report> {
        let mut found = BTreeSet::new();
        let over = at == turns.to && done >= turns.min;
        if over {
            found = self.none();
        }
        if turns.max.is_some_and(|max| done >= max) {
            return found;
        }
        let reached = ends(turns.inner, self.text, at);
        if over {
            // one more turn of nothing: the standard leaves it open
            if reached.contains(&at) {
                found.extend(self.turn(turns, at, at, &self.none()));
            }
            return found;
        }
        let fits = |end: &&usize| {
            (**end > at || done < turns.min) && self.completes(turns, **end, done + 1)
        };
        let end = *reached.iter().rev().find(fits).expect("a match");
        self.turn(turns, at, end, &self.turns(turns, end, done + 1))
    }
}

struct Turns<'a> {
    inner: &'a Node,
    min: u32,
    max: Option<u32>,
    to: usize,
}

/// Every way the groups of the match of `node` in `text` may lie.
pub fn admitted(node: &Node, groups: usize, text: &[u8], kept: Kept) -> Option<BTreeSet<Report>> {
    let (from, to) = ideal(node, text)?;
    let search = Search { text, groups, kept };
    let mut whole = vec![None; groups + 1];
    whole[0] = Some((from, to));
    Some(paired(
        &search.reports(node, from, to),
        &BTreeSet::from([whole]),
    ))
}
