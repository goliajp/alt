//! Placement of changed runs. When the lines around a run of added or
//! removed lines repeat its ends, the run can slide up or down without
//! changing what the diff means. This picks, as git does, the position a
//! reader expects: joined to a neighbouring run where possible, lined up
//! with a change on the other side, or else at the split the indentation
//! around it suggests.

use crate::myers::Side;

/// a slide is scored over at most this many positions
const MAX_SLIDING: usize = 100;
const MAX_INDENT: i32 = 200;
const MAX_BLANKS: i32 = 20;

/// A maximal run of changed lines `[start, end)`; empty between two
/// unchanged lines.
#[derive(Clone, Copy)]
struct Group {
    start: usize,
    end: usize,
}

impl Group {
    fn first(changed: &[bool]) -> Group {
        let mut g = Group { start: 0, end: 0 };
        g.grow_end(changed);
        g
    }

    fn grow_end(&mut self, changed: &[bool]) {
        while self.end < changed.len() && changed[self.end] {
            self.end += 1;
        }
    }

    fn grow_start(&mut self, changed: &[bool]) {
        while self.start > 0 && changed[self.start - 1] {
            self.start -= 1;
        }
    }

    /// Moves to the run after the next unchanged line.
    fn next(&mut self, changed: &[bool]) -> bool {
        if self.end == changed.len() {
            return false;
        }
        self.start = self.end + 1;
        self.end = self.start;
        self.grow_end(changed);
        true
    }

    /// Moves to the run before the previous unchanged line.
    fn previous(&mut self, changed: &[bool]) -> bool {
        if self.start == 0 {
            return false;
        }
        self.end = self.start - 1;
        self.start = self.end;
        self.grow_start(changed);
        true
    }
}

/// Slides the run up one line, when the line above equals its last line;
/// a run it then touches becomes part of it.
fn slide_up(side: &mut Side<'_>, g: &mut Group) -> bool {
    if g.start > 0 && side.class[g.start - 1] == side.class[g.end - 1] {
        g.start -= 1;
        g.end -= 1;
        side.changed[g.start] = true;
        side.changed[g.end] = false;
        g.grow_start(&side.changed);
        true
    } else {
        false
    }
}

/// Slides the run down one line, when the line below equals its first line.
fn slide_down(side: &mut Side<'_>, g: &mut Group) -> bool {
    if g.end < side.class.len() && side.class[g.start] == side.class[g.end] {
        side.changed[g.start] = false;
        side.changed[g.end] = true;
        g.start += 1;
        g.end += 1;
        g.grow_end(&side.changed);
        true
    } else {
        false
    }
}

/// Settles every changed run of `side`. `other` is the opposite side of the
/// same diff: its runs are walked in step, since the unchanged lines of the
/// two sides pair up one to one.
pub(crate) fn compact(side: &mut Side<'_>, other: &Side<'_>) {
    let mut g = Group::first(&side.changed);
    let mut go = Group::first(&other.changed);
    loop {
        if g.end != g.start {
            settle(side, other, &mut g, &mut go);
        }
        if !g.next(&side.changed) {
            break;
        }
        assert!(go.next(&other.changed), "sides out of step");
    }
}

fn settle(side: &mut Side<'_>, other: &Side<'_>, g: &mut Group, go: &mut Group) {
    let step_back = |go: &mut Group| assert!(go.previous(&other.changed), "sides out of step");
    let (mut size, mut earliest_end, mut end_matching_other);
    // slide all the way up, then all the way down; start over whenever
    // the run merged with a neighbour on the way
    loop {
        size = g.end - g.start;
        end_matching_other = None;
        while slide_up(side, g) {
            step_back(go);
        }
        earliest_end = g.end;
        if go.end > go.start {
            end_matching_other = Some(g.end);
        }
        while slide_down(side, g) {
            assert!(go.next(&other.changed), "sides out of step");
            if go.end > go.start {
                end_matching_other = Some(g.end);
            }
        }
        if size == g.end - g.start {
            break;
        }
    }
    if g.end == earliest_end {
        return; // the run cannot move
    }
    if end_matching_other.is_some() {
        // back up to the lowest position that lines up with a run on
        // the other side
        while go.end == go.start {
            assert!(slide_up(side, g), "run lost its match");
            step_back(go);
        }
        return;
    }
    // no run on the other side to line up with: score each position by
    // the two splits it makes, above the run and below it
    let mut shift = earliest_end
        .max((g.end - size).saturating_sub(1))
        .max(g.end.saturating_sub(MAX_SLIDING));
    let mut best: Option<(usize, Score)> = None;
    while shift <= g.end {
        let mut score = Score::default();
        score.add(&Split::measure(side, shift));
        score.add(&Split::measure(side, shift - size));
        if best.as_ref().is_none_or(|(_, b)| score.badness(b) <= 0) {
            best = Some((shift, score));
        }
        shift += 1;
    }
    let (best_shift, _) = best.expect("at least one position is scored");
    while g.end > best_shift {
        assert!(slide_up(side, g), "run cannot reach its position");
        step_back(go);
    }
}

/// The indentation of a line: columns of leading blanks, a tab reaching to
/// the next multiple of eight. `None` for a line of only whitespace.
fn indent(line: &[u8]) -> Option<i32> {
    let mut width = 0;
    for &c in line {
        match c {
            b' ' => width += 1,
            b'\t' => width += 8 - width % 8,
            b'\n' | b'\r' | 0x0b | 0x0c => {}
            _ => return Some(width),
        }
        if width >= MAX_INDENT {
            return Some(MAX_INDENT);
        }
    }
    None
}

/// What surrounds a split between two lines of one side.
struct Split {
    end_of_file: bool,
    /// indentation of the line after the split
    indent: Option<i32>,
    /// blank lines directly above the split
    pre_blank: i32,
    /// indentation of the nearest non-blank line above
    pre_indent: Option<i32>,
    /// blank lines after the line that follows the split
    post_blank: i32,
    /// indentation of the nearest non-blank line after that line
    post_indent: Option<i32>,
}

impl Split {
    fn measure(side: &Side<'_>, at: usize) -> Split {
        let lines = side.lines;
        let mut m = Split {
            end_of_file: at >= lines.len(),
            indent: lines.get(at).and_then(|l| indent(l)),
            pre_blank: 0,
            pre_indent: None,
            post_blank: 0,
            post_indent: None,
        };
        for line in lines[..at.min(lines.len())].iter().rev() {
            m.pre_indent = indent(line);
            if m.pre_indent.is_some() {
                break;
            }
            m.pre_blank += 1;
            if m.pre_blank == MAX_BLANKS {
                m.pre_indent = Some(0);
                break;
            }
        }
        for line in lines.iter().skip(at + 1) {
            m.post_indent = indent(line);
            if m.post_indent.is_some() {
                break;
            }
            m.post_blank += 1;
            if m.post_blank == MAX_BLANKS {
                m.post_indent = Some(0);
                break;
            }
        }
        m
    }
}

/// How bad a position is: lower is better on both counts.
#[derive(Default)]
struct Score {
    /// summed indentation of the lines that follow the two splits; `-1`
    /// stands for the end of the file
    effective_indent: i32,
    penalty: i32,
}

impl Score {
    fn add(&mut self, m: &Split) {
        if m.pre_indent.is_none() && m.pre_blank == 0 {
            self.penalty += 1; // start of file
        }
        if m.end_of_file {
            self.penalty += 21;
        }
        // blank lines after the split, the line right after it included
        let post_blank = if m.indent.is_none() {
            1 + m.post_blank
        } else {
            0
        };
        let total_blank = m.pre_blank + post_blank;
        // splitting next to blank lines is good, below them even better
        self.penalty += -30 * total_blank;
        self.penalty += 6 * post_blank;

        let indent = m.indent.or(m.post_indent);
        let blanks = total_blank != 0;
        self.effective_indent += indent.unwrap_or(-1);
        let (Some(indent), Some(pre_indent)) = (indent, m.pre_indent) else {
            return;
        };
        self.penalty += if indent > pre_indent {
            // the next line is nested deeper than the one above
            if blanks { 10 } else { -4 }
        } else if indent == pre_indent {
            0
        } else if m.post_indent.is_some_and(|post| post > indent) {
            // shallower, with deeper lines after: likely a block opening
            if blanks { 17 } else { 24 }
        } else {
            // shallower: likely the end of a block
            if blanks { 17 } else { 23 }
        };
    }

    /// Negative when `self` is the better position, positive when `other`.
    fn badness(&self, other: &Score) -> i32 {
        let indent = match self.effective_indent.cmp(&other.effective_indent) {
            std::cmp::Ordering::Greater => 1,
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
        };
        60 * indent + (self.penalty - other.penalty)
    }
}
