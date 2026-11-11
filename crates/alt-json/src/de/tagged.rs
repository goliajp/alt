//! Enums whose variant name is a member of the object rather than its key.
//!
//! The tag may come after the data it describes, so the data is scanned
//! once to find the tag and validate everything, then read again as the
//! variant. Errors from the second read carry no position, and content read
//! this way only has string keys: a map keyed by integers is rejected.

use super::text::Shape;
use super::{Decoder, Hidden, MapAccess, Source};
use crate::{Error, Result, Value};

/// Content that was scanned ahead and is read later.
enum Saved {
    Text(usize),
    Tree(Value),
}

enum Key {
    Tag,
    Content,
    Other,
}

impl<'a> Decoder<'a> {
    /// Reads `{"<tag>": "name", ...fields}`; the tag may be anywhere in the
    /// object. The array form `["name", ...fields]` is accepted as well.
    /// `f` reads the variant from the same object without the tag.
    pub fn internally_tagged<T>(
        &mut self,
        tag: &'static str,
        expected: &str,
        lookup: impl Fn(&str) -> Result<usize>,
        f: impl Fn(&mut Decoder<'a>, usize) -> Result<T>,
    ) -> Result<T> {
        let was_buffered = std::mem::replace(&mut self.buffered, true);
        // tags of enclosing enums stay hidden, during the scan as well
        let mut hidden = self.hidden.clone();
        let result = match self.cursor() {
            Some((start, depth)) => {
                self.scan_tag(tag, expected, &lookup)
                    .and_then(|(shape, index)| {
                        let end = self.rewind(start, depth);
                        match shape {
                            Shape::Seq => hidden.elements += 1,
                            _ => hidden.keys.push(tag),
                        }
                        self.hidden = hidden;
                        let result = f(self, index);
                        self.hidden = Hidden::default();
                        self.rewind(end, depth);
                        result.map_err(Error::unpositioned)
                    })
            }
            None => self
                .value()
                .and_then(|content| split_tag(content, tag, expected, &lookup))
                .and_then(|(content, index)| {
                    self.put(content);
                    f(self, index)
                }),
        };
        self.buffered = was_buffered;
        result
    }

    /// Position and nesting budget of the text cursor.
    fn cursor(&self) -> Option<(usize, u8)> {
        match &self.source {
            Source::Text(p) => Some((p.index, p.remaining_depth)),
            Source::Tree(_) => None,
        }
    }

    /// Moves the text cursor and returns where it was.
    fn rewind(&mut self, to: usize, depth: u8) -> usize {
        match &mut self.source {
            Source::Text(p) => {
                p.remaining_depth = depth;
                std::mem::replace(&mut p.index, to)
            }
            Source::Tree(_) => 0,
        }
    }

    fn put(&mut self, value: Value) {
        if let Source::Tree(t) = &mut self.source {
            t.put(value);
        }
    }

    /// Reads the whole value, checking all of it, and returns its tag.
    fn scan_tag(
        &mut self,
        tag: &str,
        expected: &str,
        lookup: &impl Fn(&str) -> Result<usize>,
    ) -> Result<(Shape, usize)> {
        self.structure(
            expected,
            |s| {
                let Some(index) = s.next_with(|d| d.identifier(lookup))? else {
                    return Err(Error::missing_field(tag));
                };
                while s.next_with(|d| d.value())?.is_some() {}
                Ok((Shape::Seq, index))
            },
            |m| {
                let mut index = None;
                while let Some(is_tag) = m.next_key(|key| Ok(key == tag))? {
                    if !is_tag {
                        m.next_value_with(|d| d.value())?;
                    } else if index.is_some() {
                        return Err(Error::duplicate_field(tag));
                    } else {
                        index = Some(m.next_value_with(|d| d.identifier(lookup))?);
                    }
                }
                match index {
                    Some(index) => Ok((Shape::Map, index)),
                    None => Err(Error::missing_field(tag)),
                }
            },
        )
    }

    /// The content of a variant without data: other members of the object
    /// are ignored, but the array form must have nothing after the tag.
    pub fn tagged_unit(&mut self) -> Result<()> {
        let surplus = || Error::invalid_length(1, "0 elements in sequence");
        if self.hidden.elements != 0 {
            return self.seq("unit variant", |s| match s.next_with(Decoder::ignore)? {
                Some(()) => Err(surplus()),
                None => Ok(()),
            });
        }
        match &self.source {
            Source::Text(_) => self.ignore(),
            Source::Tree(_) => match self.value()? {
                Value::Array(items) if !items.is_empty() => Err(surplus()),
                _ => Ok(()),
            },
        }
    }

    /// Reads `{"<tag>": "name", "<content>": data}` in either order, or the
    /// array `["name", data]`. `f` reads the data of the variant; its last
    /// argument is false when the object has no content member, which is
    /// how variants without data are written.
    pub fn adjacently_tagged<T>(
        &mut self,
        (tag, content): (&'static str, &'static str),
        deny_unknown_fields: bool,
        expected: &str,
        lookup: impl Fn(&str) -> Result<usize>,
        f: impl Fn(&mut Decoder<'a>, usize, bool) -> Result<T>,
    ) -> Result<T> {
        let classify = |key: &str| {
            if key == tag {
                Ok(Key::Tag)
            } else if key == content {
                Ok(Key::Content)
            } else if deny_unknown_fields {
                Err(Error::unknown_field(key, &[tag, content]))
            } else {
                Ok(Key::Other)
            }
        };
        let relevant = |m: &mut MapAccess<'_, 'a>| loop {
            match m.next_key(classify)? {
                Some(Key::Other) => m.ignore_value()?,
                key => return Ok::<_, Error>(key),
            }
        };
        self.structure(
            expected,
            |s| {
                let Some(index) = s.next_with(|d| d.identifier(&lookup))? else {
                    return Err(Error::invalid_length(0, expected));
                };
                s.next_with(|d| f(d, index, true))?
                    .ok_or_else(|| Error::invalid_length(1, expected))
            },
            |m| {
                let value = match relevant(m)? {
                    Some(Key::Tag) => {
                        let index = m.next_value_with(|d| d.variant_name(&lookup))?;
                        match relevant(m)? {
                            Some(Key::Content) => m.next_value_with(|d| f(d, index, true))?,
                            Some(_) => return Err(Error::duplicate_field(tag)),
                            None => return f(m.d, index, false),
                        }
                    }
                    Some(_) => {
                        let saved = m.next_value_with(Decoder::save)?;
                        match relevant(m)? {
                            Some(Key::Tag) => {
                                let index = m.next_value_with(|d| d.variant_name(&lookup))?;
                                m.d.replay(saved, |d| f(d, index, true))?
                            }
                            Some(_) => return Err(Error::duplicate_field(content)),
                            None => return Err(Error::missing_field(tag)),
                        }
                    }
                    None => return Err(Error::missing_field(tag)),
                };
                match relevant(m)? {
                    Some(Key::Tag) => Err(Error::duplicate_field(tag)),
                    Some(_) => Err(Error::duplicate_field(content)),
                    None => Ok(value),
                }
            },
        )
    }

    /// The tag member of an adjacently tagged enum: a variant name in either
    /// form an enum without data can take, `"name"` or `{"name": null}`.
    /// The array form takes a plain string only.
    fn variant_name(&mut self, lookup: &impl Fn(&str) -> Result<usize>) -> Result<usize> {
        self.enumeration(lookup, |v, index| v.unit().map(|()| index))
    }

    /// Checks the next value and remembers it for [`Decoder::replay`].
    fn save(&mut self) -> Result<Saved> {
        match &mut self.source {
            Source::Text(p) => {
                let start = p.index;
                p.read_value()?;
                Ok(Saved::Text(start))
            }
            Source::Tree(t) => t.take().map(Saved::Tree),
        }
    }

    fn replay<T>(
        &mut self,
        saved: Saved,
        f: impl FnOnce(&mut Decoder<'a>) -> Result<T>,
    ) -> Result<T> {
        let was_buffered = std::mem::replace(&mut self.buffered, true);
        let result = match saved {
            Saved::Text(start) => {
                let depth = self.cursor().map_or(0, |(_, depth)| depth);
                let resume = self.rewind(start, depth);
                let result = f(self);
                self.rewind(resume, depth);
                result.map_err(Error::unpositioned)
            }
            Saved::Tree(value) => {
                self.put(value);
                f(self)
            }
        };
        self.buffered = was_buffered;
        result
    }
}

/// Takes the tag out of a tree and resolves it.
fn split_tag(
    content: Value,
    tag: &str,
    expected: &str,
    lookup: &impl Fn(&str) -> Result<usize>,
) -> Result<(Value, usize)> {
    let (name, rest) = match content {
        Value::Object(mut map) => (map.remove(tag), Value::Object(map)),
        Value::Array(mut items) if !items.is_empty() => {
            (Some(items.remove(0)), Value::Array(items))
        }
        Value::Array(_) => (None, Value::Null),
        other => return Err(Error::invalid_type(super::tree::describe(&other), expected)),
    };
    match name {
        Some(Value::String(name)) => Ok((rest, lookup(&name)?)),
        Some(other) => Err(Error::invalid_type(
            super::tree::describe(&other),
            "variant identifier",
        )),
        None => Err(Error::missing_field(tag)),
    }
}
