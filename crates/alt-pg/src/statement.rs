//! Prepared statements kept per connection, keyed by their SQL text.

use crate::{Column, Oid};
use std::{
    collections::{HashMap, VecDeque},
    hash::{BuildHasherDefault, Hasher},
    sync::Arc,
};

/// the most statements one connection keeps prepared
pub const STATEMENT_LIMIT: usize = 256;

pub(crate) struct Statement {
    /// zero for the unnamed statement
    pub id: u32,
    pub params: Box<[Oid]>,
    pub columns: Arc<[Column]>,
}

/// SQL text comes from the program, not from its input, so the map does not
/// need a keyed hash
#[derive(Default)]
pub(crate) struct TextHasher(u64);

impl Hasher for TextHasher {
    fn write(&mut self, bytes: &[u8]) {
        const MULTIPLIER: u64 = 0x9e37_79b9_7f4a_7c15;
        let (words, tail) = bytes.as_chunks::<8>();
        let mut state = self.0;
        for word in words {
            state = (state.rotate_left(5) ^ u64::from_le_bytes(*word)).wrapping_mul(MULTIPLIER);
        }
        let mut last = [0u8; 8];
        last[..tail.len()].copy_from_slice(tail);
        last[7] = tail.len() as u8;
        self.0 = (state.rotate_left(5) ^ u64::from_le_bytes(last)).wrapping_mul(MULTIPLIER);
    }
    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 29)
    }
}

#[derive(Default)]
pub(crate) struct Statements {
    values: HashMap<Box<str>, Statement, BuildHasherDefault<TextHasher>>,
    order: VecDeque<Box<str>>,
    last: u32,
}

impl Statements {
    pub fn get(&self, sql: &str) -> Option<&Statement> {
        self.values.get(sql)
    }
    pub fn len(&self) -> usize {
        self.values.len()
    }
    /// a name no statement of this connection has used
    pub fn next_id(&mut self) -> u32 {
        self.last = self.last.checked_add(1).expect("statement names exhausted");
        self.last
    }
    /// keeps `statement` and returns the one it displaced, oldest first
    pub fn insert(&mut self, sql: &str, statement: Statement) -> Option<u32> {
        let displaced = if self.values.len() == STATEMENT_LIMIT {
            let oldest = self.order.pop_front().expect("statement cache order");
            self.values.remove(&oldest).map(|statement| statement.id)
        } else {
            None
        };
        self.order.push_back(sql.into());
        self.values.insert(sql.into(), statement);
        displaced
    }
    /// forgets every statement and returns their names
    pub fn clear(&mut self) -> impl Iterator<Item = u32> {
        self.order.clear();
        self.values.drain().map(|(_, statement)| statement.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(id: u32) -> Statement {
        Statement {
            id,
            params: Box::new([]),
            columns: Arc::from(Vec::new()),
        }
    }

    #[test]
    fn the_oldest_statement_is_displaced_at_the_limit() {
        let mut statements = Statements::default();
        for index in 0..STATEMENT_LIMIT {
            let id = statements.next_id();
            assert_eq!(
                statements.insert(&format!("SELECT {index}"), statement(id)),
                None
            );
        }
        let id = statements.next_id();
        assert_eq!(statements.insert("SELECT x", statement(id)), Some(1));
        assert_eq!(statements.len(), STATEMENT_LIMIT);
        assert!(statements.get("SELECT 0").is_none());
        assert_eq!(statements.get("SELECT 1").unwrap().id, 2);
        assert_eq!(statements.get("SELECT x").unwrap().id, id);
        assert_eq!(statements.clear().count(), STATEMENT_LIMIT);
        assert_eq!(statements.len(), 0);
        assert_eq!(statements.next_id(), id + 1);
    }

    #[test]
    fn texts_differing_anywhere_hash_apart() {
        let hash = |text: &str| {
            let mut hasher = TextHasher::default();
            hasher.write(text.as_bytes());
            hasher.finish()
        };
        let texts = [
            "",
            "a",
            "b",
            "SELECT 1",
            "SELECT 2",
            "SELECT 1 ",
            "SELECT value FROM t WHERE id=$1",
            "SELECT value FROM t WHERE id=$2",
            "TELECT value FROM t WHERE id=$1",
        ];
        for (index, left) in texts.iter().enumerate() {
            for right in &texts[index + 1..] {
                assert_ne!(hash(left), hash(right), "{left:?} {right:?}");
            }
        }
    }
}
