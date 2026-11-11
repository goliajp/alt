//! Arithmetic in GF(2^255 - 19) on five 51-bit limbs.
//!
//! Limbs may exceed 51 bits between operations. `mul` and `square` accept
//! limbs below 2^54 and return limbs below 2^52; `add` does not carry, so
//! at most two additions may be stacked before the next multiplication.

const MASK: u64 = (1 << 51) - 1;

#[derive(Clone, Copy)]
pub(super) struct Fe(pub(super) [u64; 5]);

/// the curve constant d = -121665 / 121666
pub(super) const D: Fe = Fe([
    929955233495203,
    466365720129213,
    1662059464998953,
    2033849074728123,
    1442794654840575,
]);
pub(super) const D2: Fe = Fe([
    1859910466990425,
    932731440258426,
    1072319116312658,
    1815898335770999,
    633789495995903,
]);
/// a square root of -1
pub(super) const SQRT_M1: Fe = Fe([
    1718705420411056,
    234908883556509,
    2233514472574048,
    2117202627021982,
    765476049583133,
]);

#[inline(always)]
fn m(a: u64, b: u64) -> u128 {
    a as u128 * b as u128
}

/// carries five wide limbs down to limbs below 2^52
#[inline(always)]
fn carry(c: [u128; 5]) -> Fe {
    let c1 = c[1] + (c[0] >> 51);
    let c2 = c[2] + (c1 >> 51);
    let c3 = c[3] + (c2 >> 51);
    let c4 = c[4] + (c3 >> 51);
    let mut out = [
        c[0] as u64 & MASK,
        c1 as u64 & MASK,
        c2 as u64 & MASK,
        c3 as u64 & MASK,
        c4 as u64 & MASK,
    ];
    out[0] += (c4 >> 51) as u64 * 19;
    out[1] += out[0] >> 51;
    out[0] &= MASK;
    Fe(out)
}

impl Fe {
    pub(super) const ZERO: Fe = Fe([0; 5]);
    pub(super) const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    /// reads 255 bits; the top bit of the last byte is ignored and values
    /// of p and above are accepted as they are
    pub(super) fn from_bytes(bytes: &[u8; 32]) -> Fe {
        let load = |at: usize| u64::from_le_bytes(*bytes[at..].first_chunk().expect("in range"));
        Fe([
            load(0) & MASK,
            (load(6) >> 3) & MASK,
            (load(12) >> 6) & MASK,
            (load(19) >> 1) & MASK,
            (load(24) >> 12) & MASK,
        ])
    }

    /// the unique encoding of the value reduced below p
    pub(super) fn to_bytes(self) -> [u8; 32] {
        let mut l = self.weak_reduce().0;
        // q is 1 exactly when the value is at least p
        let mut q = (l[0] + 19) >> 51;
        for limb in &l[1..] {
            q = (limb + q) >> 51;
        }
        l[0] += 19 * q;
        for i in 0..4 {
            l[i + 1] += l[i] >> 51;
            l[i] &= MASK;
        }
        l[4] &= MASK;
        let mut out = [0u8; 32];
        let low = l[0] as u128 | (l[1] as u128) << 51 | (l[2] as u128) << 102;
        let high = (l[2] >> 26) as u128 | (l[3] as u128) << 25 | (l[4] as u128) << 76;
        out[..16].copy_from_slice(&low.to_le_bytes());
        out[16..].copy_from_slice(&high.to_le_bytes());
        out
    }

    #[inline(always)]
    fn weak_reduce(self) -> Fe {
        let l = self.0;
        Fe([
            (l[0] & MASK) + (l[4] >> 51) * 19,
            (l[1] & MASK) + (l[0] >> 51),
            (l[2] & MASK) + (l[1] >> 51),
            (l[3] & MASK) + (l[2] >> 51),
            (l[4] & MASK) + (l[3] >> 51),
        ])
    }

    #[inline(always)]
    pub(super) fn add(&self, rhs: &Fe) -> Fe {
        let (a, b) = (&self.0, &rhs.0);
        Fe([
            a[0] + b[0],
            a[1] + b[1],
            a[2] + b[2],
            a[3] + b[3],
            a[4] + b[4],
        ])
    }

    /// `rhs` limbs must be below 2^55; 16p is added first so no limb underflows
    #[inline(always)]
    pub(super) fn sub(&self, rhs: &Fe) -> Fe {
        let (a, b) = (&self.0, &rhs.0);
        Fe([
            (a[0] + 36028797018963664) - b[0],
            (a[1] + 36028797018963952) - b[1],
            (a[2] + 36028797018963952) - b[2],
            (a[3] + 36028797018963952) - b[3],
            (a[4] + 36028797018963952) - b[4],
        ])
        .weak_reduce()
    }

    #[inline(always)]
    pub(super) fn neg(&self) -> Fe {
        Fe::ZERO.sub(self)
    }

    #[inline(always)]
    pub(super) fn mul(&self, rhs: &Fe) -> Fe {
        let (a, b) = (&self.0, &rhs.0);
        let b1 = b[1] * 19;
        let b2 = b[2] * 19;
        let b3 = b[3] * 19;
        let b4 = b[4] * 19;
        carry([
            m(a[0], b[0]) + m(a[4], b1) + m(a[3], b2) + m(a[2], b3) + m(a[1], b4),
            m(a[1], b[0]) + m(a[0], b[1]) + m(a[4], b2) + m(a[3], b3) + m(a[2], b4),
            m(a[2], b[0]) + m(a[1], b[1]) + m(a[0], b[2]) + m(a[4], b3) + m(a[3], b4),
            m(a[3], b[0]) + m(a[2], b[1]) + m(a[1], b[2]) + m(a[0], b[3]) + m(a[4], b4),
            m(a[4], b[0]) + m(a[3], b[1]) + m(a[2], b[2]) + m(a[1], b[3]) + m(a[0], b[4]),
        ])
    }

    #[inline(always)]
    fn square_wide(&self) -> [u128; 5] {
        let a = &self.0;
        let a3 = a[3] * 19;
        let a4 = a[4] * 19;
        [
            m(a[0], a[0]) + 2 * (m(a[1], a4) + m(a[2], a3)),
            m(a[3], a3) + 2 * (m(a[0], a[1]) + m(a[2], a4)),
            m(a[1], a[1]) + 2 * (m(a[0], a[2]) + m(a[4], a3)),
            m(a[4], a4) + 2 * (m(a[0], a[3]) + m(a[1], a[2])),
            m(a[2], a[2]) + 2 * (m(a[0], a[4]) + m(a[1], a[3])),
        ]
    }

    #[inline(always)]
    pub(super) fn square(&self) -> Fe {
        carry(self.square_wide())
    }

    /// twice the square
    #[inline(always)]
    pub(super) fn square2(&self) -> Fe {
        carry(self.square_wide().map(|limb| limb * 2))
    }

    /// squares `count` times
    fn pow2k(&self, count: u32) -> Fe {
        let mut out = *self;
        for _ in 0..count {
            out = out.square();
        }
        out
    }

    /// (self^(2^250 - 1), self^11)
    fn pow22501(&self) -> (Fe, Fe) {
        let t0 = self.square();
        let t1 = t0.pow2k(2);
        let t2 = self.mul(&t1);
        let t3 = t0.mul(&t2);
        let t4 = t3.square();
        let t5 = t2.mul(&t4);
        let t7 = t5.pow2k(5).mul(&t5);
        let t9 = t7.pow2k(10).mul(&t7);
        let t11 = t9.pow2k(20).mul(&t9);
        let t13 = t11.pow2k(10).mul(&t7);
        let t15 = t13.pow2k(50).mul(&t13);
        let t17 = t15.pow2k(100).mul(&t15);
        let t19 = t17.pow2k(50).mul(&t13);
        (t19, t3)
    }

    /// the inverse, or zero for zero
    pub(super) fn invert(&self) -> Fe {
        let (t19, t3) = self.pow22501();
        t19.pow2k(5).mul(&t3)
    }

    /// self^((p - 5) / 8)
    fn pow_p58(&self) -> Fe {
        self.pow22501().0.pow2k(2).mul(self)
    }

    /// the low bit of the canonical encoding, the "sign" of an x coordinate
    pub(super) fn is_negative(&self) -> bool {
        self.to_bytes()[0] & 1 == 1
    }

    pub(super) fn is_zero(&self) -> bool {
        self.to_bytes() == [0; 32]
    }

    /// `other` where every bit of `mask` is set, `self` where none is
    #[inline(always)]
    pub(super) fn select(&self, other: &Fe, mask: u64) -> Fe {
        let mut out = self.0;
        for (limb, theirs) in out.iter_mut().zip(other.0) {
            *limb ^= mask & (*limb ^ theirs);
        }
        Fe(out)
    }

    /// The nonnegative square root of u / v when there is one. Not constant
    /// time: only public values are decoded.
    pub(super) fn sqrt_ratio(u: &Fe, v: &Fe) -> Option<Fe> {
        let v3 = v.square().mul(v);
        let v7 = v3.square().mul(v);
        let mut r = u.mul(&v3).mul(&u.mul(&v7).pow_p58());
        let check = v.mul(&r.square()).to_bytes();
        // r^2 * v is u, -u, or one of those times sqrt(-1); only the first two are squares
        if check == u.neg().to_bytes() {
            r = r.mul(&SQRT_M1);
        } else if check != u.to_bytes() {
            return None;
        }
        if r.is_negative() {
            r = r.neg();
        }
        Some(r)
    }
}
