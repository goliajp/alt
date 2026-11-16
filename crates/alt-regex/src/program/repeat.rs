//! A repetition as the copies it is made of.

use super::{Compiler, Inst, nullable};
use crate::Error;
use crate::syntax::Node;

impl Compiler {
    /// One of the copies a repetition is made of. The C library marks
    /// a group that a repetition may leave out, the first such copy
    /// only, and a copy made of another loses the marks inside it.
    fn copy(&mut self, inner: &Node, next: u32, marked: bool, made: bool) -> Result<u32, Error> {
        let outside = self.copied;
        self.copied = outside || made;
        let entry = match inner {
            Node::Group(body, number) if marked && !outside && nullable(body) => {
                let close = self.push(Inst::SaveOpt(2 * number + 1, next))?;
                let body = self.node(body, close)?;
                self.push(Inst::Save(2 * number, body))
            }
            _ => self.node(inner, next),
        };
        self.copied = outside;
        entry
    }

    pub(super) fn repeat(
        &mut self,
        inner: &Node,
        min: u32,
        max: Option<u32>,
        next: u32,
    ) -> Result<u32, Error> {
        let mut entry = match max {
            // the loop: once more, or on
            None if !nullable(inner) => {
                let split = self.push(Inst::Split(next, next))?;
                let body = self.copy(inner, split, false, min > 0)?;
                self.insts[split as usize] = Inst::Split(body, next);
                split
            }
            // a turn that takes nothing comes back to where it began, and
            // no path goes through an instruction twice at one place:
            // behind a turn the choice is made at an instruction of its
            // own, so that one such turn is taken and then left
            None => {
                let again = self.push(Inst::Split(next, next))?;
                let body = self.copy(inner, again, true, min > 0)?;
                self.insts[again as usize] = Inst::Split(body, next);
                self.push(Inst::Split(body, next))?
            }
            // the optional copies, wired as the C library wires them:
            // of fewer copies than there may be, the last ones are the
            // ones taken, each entered only behind the one before it
            Some(max) => {
                let mut bodies = Vec::new();
                let mut behind = next;
                for copy in (min..max).rev() {
                    let first = copy == min;
                    behind = self.copy(inner, behind, first, !first || min > 0)?;
                    bodies.push(behind);
                }
                bodies.reverse();
                let mut entry = next;
                for (at, body) in bodies.iter().enumerate() {
                    let without = bodies.get(at + 1).copied().unwrap_or(next);
                    let with = if at == 0 { *body } else { entry };
                    entry = self.push(Inst::Split(with, without))?;
                }
                entry
            }
        };
        for copy in (0..min).rev() {
            entry = self.copy(inner, entry, false, copy > 0)?;
        }
        Ok(entry)
    }
}
