//! The tree a pattern is read into.

use std::sync::Arc;

use crate::budget::Budget;
use crate::class::ByteSet;

/// A condition on a position that consumes nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    TextStart,
    TextEnd,
    LineStart,
    LineEnd,
    WordBoundary,
    NotWordBoundary,
    WordStart,
    WordEnd,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Empty,
    Set(ByteSet),
    /// One character of a set that reaches beyond ASCII, in a UTF-8
    /// text.
    Class(Arc<crate::block::Block>),
    Look(Look),
    /// A parenthesized subexpression and its number, from 1.
    Group(Box<Node>, u32),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    /// What the group of this number matched, once more.
    BackRef(u32),
    /// At least `.1` and at most `.2` repetitions.
    Repeat(Box<Node>, u32, Option<u32>),
}

impl Node {
    /// Whether nothing was written, or only repetitions of none.
    pub fn nothing(&self) -> bool {
        match self {
            Node::Empty => true,
            Node::Concat(items) => items.iter().all(Node::nothing),
            _ => false,
        }
    }

    /// Whether a group is written in it.
    pub fn grouped(&self) -> bool {
        match self {
            Node::Group(..) => true,
            Node::Repeat(inner, ..) => inner.grouped(),
            Node::Concat(items) | Node::Alt(items) => items.iter().any(Node::grouped),
            _ => false,
        }
    }

    /// The bytes held behind the node. A set of characters that other
    /// patterns share is not counted: it is held whether or not this
    /// one is.
    pub fn held(&self) -> usize {
        let all = |items: &[Node]| items.iter().map(Node::held).sum::<usize>();
        match self {
            Node::Class(block) if Arc::strong_count(block) == 1 => block.bytes(),
            Node::Concat(items) | Node::Alt(items) => {
                items.capacity() * size_of::<Node>() + all(items)
            }
            Node::Group(inner, _) | Node::Repeat(inner, ..) => size_of::<Node>() + inner.held(),
            _ => 0,
        }
    }
}

/// What the matchers need to know of a pattern besides its tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Shape {
    /// The bytes the tree holds.
    pub budget: Budget,
    /// The C library reads the pattern character by character, so a
    /// match begins only where a character does. Otherwise it matches
    /// bytes, also a UTF-8 text.
    pub apart: bool,
    /// A byte written in the pattern goes on a character and begins
    /// none.
    pub strays: bool,
}
