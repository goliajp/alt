//! The map behind [`Value::Object`]: keys are kept sorted.

use std::borrow::Borrow;
use std::collections::{BTreeMap, btree_map};
use std::fmt;
use std::ops;

use crate::Value;

pub use std::collections::btree_map::{
    Entry, IntoIter, IntoValues, Iter, IterMut, Keys, OccupiedEntry, VacantEntry, Values, ValuesMut,
};

/// A JSON object. Iteration and output go in ascending key order, comparing
/// keys byte by byte.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Map<K = String, V = Value> {
    map: BTreeMap<K, V>,
}

impl Map<String, Value> {
    pub const fn new() -> Self {
        Map {
            map: BTreeMap::new(),
        }
    }

    /// The capacity is ignored: a sorted map allocates per node.
    pub fn with_capacity(_capacity: usize) -> Self {
        Map::new()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&Value>
    where
        String: Borrow<Q>,
        Q: ?Sized + Ord,
    {
        self.map.get(key)
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        String: Borrow<Q>,
        Q: ?Sized + Ord,
    {
        self.map.contains_key(key)
    }

    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut Value>
    where
        String: Borrow<Q>,
        Q: ?Sized + Ord,
    {
        self.map.get_mut(key)
    }

    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(&String, &Value)>
    where
        String: Borrow<Q>,
        Q: ?Sized + Ord,
    {
        self.map.get_key_value(key)
    }

    /// Returns the value the key had before, if any.
    pub fn insert(&mut self, k: String, v: Value) -> Option<Value> {
        self.map.insert(k, v)
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<Value>
    where
        String: Borrow<Q>,
        Q: ?Sized + Ord,
    {
        self.map.remove(key)
    }

    pub fn remove_entry<Q>(&mut self, key: &Q) -> Option<(String, Value)>
    where
        String: Borrow<Q>,
        Q: ?Sized + Ord,
    {
        self.map.remove_entry(key)
    }

    /// Moves every entry of `other` into `self`, leaving `other` empty.
    pub fn append(&mut self, other: &mut Self) {
        self.map.append(&mut other.map);
    }

    pub fn entry<S>(&mut self, key: S) -> Entry<'_, String, Value>
    where
        S: Into<String>,
    {
        self.map.entry(key.into())
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn iter(&self) -> Iter<'_, String, Value> {
        self.map.iter()
    }

    pub fn iter_mut(&mut self) -> IterMut<'_, String, Value> {
        self.map.iter_mut()
    }

    pub fn keys(&self) -> Keys<'_, String, Value> {
        self.map.keys()
    }

    pub fn values(&self) -> Values<'_, String, Value> {
        self.map.values()
    }

    pub fn values_mut(&mut self) -> ValuesMut<'_, String, Value> {
        self.map.values_mut()
    }

    pub fn into_values(self) -> IntoValues<String, Value> {
        self.map.into_values()
    }

    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&String, &mut Value) -> bool,
    {
        self.map.retain(f);
    }
}

impl Default for Map<String, Value> {
    fn default() -> Self {
        Map::new()
    }
}

impl fmt::Debug for Map<String, Value> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.map.fmt(f)
    }
}

/// Panics when the key is absent.
impl<Q> ops::Index<&Q> for Map<String, Value>
where
    String: Borrow<Q>,
    Q: ?Sized + Ord,
{
    type Output = Value;

    fn index(&self, index: &Q) -> &Value {
        self.map.index(index)
    }
}

/// Panics when the key is absent.
impl<Q> ops::IndexMut<&Q> for Map<String, Value>
where
    String: Borrow<Q>,
    Q: ?Sized + Ord,
{
    fn index_mut(&mut self, index: &Q) -> &mut Value {
        self.map.get_mut(index).expect("no entry found for key")
    }
}

impl FromIterator<(String, Value)> for Map<String, Value> {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = (String, Value)>,
    {
        Map {
            map: FromIterator::from_iter(iter),
        }
    }
}

impl Extend<(String, Value)> for Map<String, Value> {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = (String, Value)>,
    {
        self.map.extend(iter);
    }
}

impl<'a> IntoIterator for &'a Map<String, Value> {
    type Item = (&'a String, &'a Value);
    type IntoIter = btree_map::Iter<'a, String, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

impl<'a> IntoIterator for &'a mut Map<String, Value> {
    type Item = (&'a String, &'a mut Value);
    type IntoIter = btree_map::IterMut<'a, String, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.iter_mut()
    }
}

impl IntoIterator for Map<String, Value> {
    type Item = (String, Value);
    type IntoIter = btree_map::IntoIter<String, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.into_iter()
    }
}
