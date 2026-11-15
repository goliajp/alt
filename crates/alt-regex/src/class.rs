//! Sets of bytes: what one position of a pattern accepts.

/// A set of byte values.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct ByteSet([u64; 4]);

impl ByteSet {
    pub const fn empty() -> Self {
        ByteSet([0; 4])
    }

    pub const fn full() -> Self {
        ByteSet([u64::MAX; 4])
    }

    pub fn byte(b: u8) -> Self {
        let mut set = Self::empty();
        set.insert(b);
        set
    }

    pub fn of(bytes: &[u8]) -> Self {
        let mut set = Self::empty();
        for &b in bytes {
            set.insert(b);
        }
        set
    }

    pub fn insert(&mut self, b: u8) {
        self.0[usize::from(b >> 6)] |= 1 << (b & 63);
    }

    pub fn remove(&mut self, b: u8) {
        self.0[usize::from(b >> 6)] &= !(1 << (b & 63));
    }

    pub fn insert_range(&mut self, from: u8, to: u8) {
        for b in from..=to {
            self.insert(b);
        }
    }

    pub fn contains(&self, b: u8) -> bool {
        self.0[usize::from(b >> 6)] >> (b & 63) & 1 == 1
    }

    pub fn union(&mut self, other: &ByteSet) {
        for (mine, theirs) in self.0.iter_mut().zip(other.0) {
            *mine |= theirs;
        }
    }

    pub fn negate(&mut self) {
        for word in &mut self.0 {
            *word = !*word;
        }
    }

    pub fn len(&self) -> u32 {
        self.0.iter().map(|w| w.count_ones()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.0 == [0; 4]
    }

    /// The one byte of a set that holds exactly one.
    pub fn single(&self) -> Option<u8> {
        if self.len() != 1 {
            return None;
        }
        (0..=255).find(|&b| self.contains(b))
    }

    /// Adds the other case of every ASCII letter in the set.
    pub fn fold_case(&mut self) {
        for b in b'a'..=b'z' {
            let upper = b.to_ascii_uppercase();
            if self.contains(b) || self.contains(upper) {
                self.insert(b);
                self.insert(upper);
            }
        }
    }

    /// The bytes that make up a word: letters, digits and `_`.
    pub fn word() -> Self {
        let mut set = Self::class(b"alnum").expect("a known class");
        set.insert(b'_');
        set
    }

    /// A POSIX character class by name, as the C locale defines it.
    pub fn class(name: &[u8]) -> Option<Self> {
        let mut set = Self::empty();
        match name {
            b"alpha" => {
                set.insert_range(b'a', b'z');
                set.insert_range(b'A', b'Z');
            }
            b"digit" => set.insert_range(b'0', b'9'),
            b"alnum" => {
                set.insert_range(b'a', b'z');
                set.insert_range(b'A', b'Z');
                set.insert_range(b'0', b'9');
            }
            b"upper" => set.insert_range(b'A', b'Z'),
            b"lower" => set.insert_range(b'a', b'z'),
            b"space" => set = Self::of(b" \t\n\r\x0b\x0c"),
            b"blank" => set = Self::of(b" \t"),
            b"punct" => {
                set.insert_range(b'!', b'/');
                set.insert_range(b':', b'@');
                set.insert_range(b'[', b'`');
                set.insert_range(b'{', b'~');
            }
            b"print" => set.insert_range(b' ', b'~'),
            b"graph" => set.insert_range(b'!', b'~'),
            b"cntrl" => {
                set.insert_range(0, 0x1f);
                set.insert(0x7f);
            }
            b"xdigit" => {
                set.insert_range(b'0', b'9');
                set.insert_range(b'a', b'f');
                set.insert_range(b'A', b'F');
            }
            _ => return None,
        }
        Some(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_adds_the_other_case_of_letters_only() {
        let mut set = ByteSet::of(b"aZ1[");
        set.fold_case();
        for b in *b"aAzZ1[" {
            assert!(set.contains(b), "{}", b as char);
        }
        assert_eq!(set.len(), 6);
    }

    #[test]
    fn a_negated_set_holds_exactly_the_other_bytes() {
        let mut set = ByteSet::class(b"digit").unwrap();
        assert_eq!(set.len(), 10);
        set.negate();
        assert_eq!(set.len(), 246);
        assert!(!set.contains(b'5') && set.contains(b'a') && set.contains(0xff));
        assert_eq!(ByteSet::byte(b'x').single(), Some(b'x'));
        assert_eq!(set.single(), None);
    }
}
