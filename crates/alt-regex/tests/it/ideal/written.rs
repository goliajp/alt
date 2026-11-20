//! Random patterns and texts for the comparisons with the rules.

pub struct Rng(pub u64);

impl Rng {
    pub fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    pub fn pick<'a>(&mut self, of: &[&'a [u8]]) -> &'a [u8] {
        of[self.below(of.len())]
    }
}

const ATOMS: [&[u8]; 14] = [
    b"a",
    b"b",
    b"a",
    b"x",
    b" ",
    b"ab",
    b".",
    b"[ab]",
    b"[^a]",
    b"[a-c]",
    b"[[:alpha:]]",
    b"[^[:space:]b]",
    b"\\w",
    b"\\W",
];
const EDGES: [&[u8]; 6] = [b"^", b"$", b"\\b", b"\\B", b"\\<", b"\\>"];

/// Writes the patterns the comparison with the C library leaves out.
pub struct Writer<'a> {
    pub rng: &'a mut Rng,
    /// whether anchors and the edges of words are written
    pub edges: bool,
    pub out: Vec<u8>,
}

impl Writer<'_> {
    fn atom(&mut self, depth: u32) {
        match self.rng.below(if depth == 0 { 8 } else { 12 }) {
            0..=6 => {
                let atom = self.rng.pick(&ATOMS);
                self.out.extend(atom);
            }
            7 if self.edges => {
                // an edge cannot be repeated: an atom goes behind it
                let edge = self.rng.pick(&EDGES);
                self.out.extend(edge);
                self.out.push(b'a');
            }
            7 => self.out.extend(b"b"),
            8 => self.out.extend(b"()"),
            _ => {
                self.out.push(b'(');
                self.expression(depth - 1);
                self.out.push(b')');
            }
        }
    }

    fn branch(&mut self, depth: u32) {
        let repeats: [&[u8]; 11] = [
            b"", b"", b"", b"?", b"{2}", b"{1,2}", b"{0,1}", b"{,2}", b"*", b"+", b"{2,}",
        ];
        for _ in 0..self.rng.below(4) {
            self.atom(depth);
            let repeat = self.rng.pick(&repeats);
            self.out.extend(repeat);
            if self.edges && self.rng.below(4) == 0 {
                let edge = self.rng.pick(&EDGES);
                self.out.extend(edge);
            }
        }
    }

    pub fn expression(&mut self, depth: u32) {
        self.branch(depth);
        if self.rng.below(3) == 0 {
            self.out.push(b'|');
            self.branch(depth);
        }
    }
}

pub fn text(rng: &mut Rng) -> Vec<u8> {
    let pieces: [&[u8]; 8] = [b"a", b"a", b"b", b"b", b"x", b" ", b"A", b"_"];
    (0..rng.below(7))
        .flat_map(|_| rng.pick(&pieces))
        .copied()
        .collect()
}
