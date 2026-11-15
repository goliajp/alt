//! The search at the heart of the Myers diff: a shortest edit path is
//! found by cutting the problem in two at a point on that path, located
//! by growing paths from both ends until they meet. Large, costly inputs
//! are cut early at a point that merely looks good, as git does.

use crate::myers::Kept;

/// Search limits, fixed for one diff.
struct Limits {
    /// edit cost after which the search settles for the furthest path
    max_cost: i64,
    /// a diagonal run this long counts as a good partial match
    snake: i64,
    /// edit cost before which the partial-match shortcut is not tried
    heuristic_min: i64,
}

const MAX_COST_MIN: i64 = 256;
const HEURISTIC_MIN_COST: i64 = 256;
const SNAKE: i64 = 20;
const HEURISTIC_FACTOR: i64 = 4;

/// Marks in `changed1` and `changed2` the kept lines that differ.
pub(crate) fn search(one: &Kept, two: &Kept, changed1: &mut [bool], changed2: &mut [bool]) {
    let diagonals = one.class.len() + two.class.len() + 3;
    let mut state = Search {
        one,
        two,
        changed1,
        changed2,
        // forward and backward furthest-reaching points, one slot per
        // diagonal, with room for one diagonal outside the box on each end
        forward: vec![0; diagonals + 1],
        backward: vec![0; diagonals + 1],
        zero: two.class.len() as i64 + 1,
        limits: Limits {
            max_cost: (rough_sqrt(diagonals) as i64).max(MAX_COST_MIN),
            snake: SNAKE,
            heuristic_min: HEURISTIC_MIN_COST,
        },
    };
    state.compare(0, one.class.len() as i64, 0, two.class.len() as i64, false);
}

/// Number of bits in `n`, halved and rounded up, as a power of two: a cheap
/// stand-in for a square root.
pub(crate) fn rough_sqrt(mut n: usize) -> usize {
    let mut root = 1;
    while n > 0 {
        root <<= 1;
        n >>= 2;
    }
    root
}

struct Search<'s> {
    one: &'s Kept,
    two: &'s Kept,
    changed1: &'s mut [bool],
    changed2: &'s mut [bool],
    forward: Vec<i64>,
    backward: Vec<i64>,
    /// slot of diagonal 0 in `forward` and `backward`
    zero: i64,
    limits: Limits,
}

/// Where a box is cut in two, and whether each half must be solved exactly.
struct Split {
    i1: i64,
    i2: i64,
    exact_low: bool,
    exact_high: bool,
}

impl Search<'_> {
    fn eq(&self, i1: i64, i2: i64) -> bool {
        self.one.class[i1 as usize] == self.two.class[i2 as usize]
    }

    fn f(&self, d: i64) -> i64 {
        self.forward[(self.zero + d) as usize]
    }

    fn b(&self, d: i64) -> i64 {
        self.backward[(self.zero + d) as usize]
    }

    fn set_f(&mut self, d: i64, v: i64) {
        self.forward[(self.zero + d) as usize] = v;
    }

    fn set_b(&mut self, d: i64, v: i64) {
        self.backward[(self.zero + d) as usize] = v;
    }

    /// Diffs kept lines `[off1, lim1)` against `[off2, lim2)`.
    fn compare(
        &mut self,
        mut off1: i64,
        mut lim1: i64,
        mut off2: i64,
        mut lim2: i64,
        mut exact: bool,
    ) {
        loop {
            while off1 < lim1 && off2 < lim2 && self.eq(off1, off2) {
                off1 += 1;
                off2 += 1;
            }
            while off1 < lim1 && off2 < lim2 && self.eq(lim1 - 1, lim2 - 1) {
                lim1 -= 1;
                lim2 -= 1;
            }
            if off1 == lim1 {
                for i in off2..lim2 {
                    self.changed2[self.two.index[i as usize]] = true;
                }
                return;
            }
            if off2 == lim2 {
                for i in off1..lim1 {
                    self.changed1[self.one.index[i as usize]] = true;
                }
                return;
            }
            let split = self.split(off1, lim1, off2, lim2, exact);
            self.compare(off1, split.i1, off2, split.i2, split.exact_low);
            (off1, off2, exact) = (split.i1, split.i2, split.exact_high);
        }
    }

    /// Finds a point on a short edit path through the box by growing paths
    /// from both corners until they meet. For a costly box that is allowed
    /// to be inexact, it returns early with a point that merely looks good.
    fn split(&mut self, off1: i64, lim1: i64, off2: i64, lim2: i64, exact: bool) -> Split {
        let (dmin, dmax) = (off1 - lim2, lim1 - off2);
        let (fmid, bmid) = (off1 - off2, lim1 - lim2);
        let odd = (fmid - bmid) & 1 != 0;
        let (mut fmin, mut fmax, mut bmin, mut bmax) = (fmid, fmid, bmid, bmid);
        let met = |i1, i2| Split {
            i1,
            i2,
            exact_low: true,
            exact_high: true,
        };
        self.set_f(fmid, off1);
        self.set_b(bmid, lim1);

        let mut cost = 1;
        loop {
            let mut long_run = false;

            // widen the forward diagonals by one each way, or narrow them
            // where the box ends
            if fmin > dmin {
                fmin -= 1;
                self.set_f(fmin - 1, -1);
            } else {
                fmin += 1;
            }
            if fmax < dmax {
                fmax += 1;
                self.set_f(fmax + 1, -1);
            } else {
                fmax -= 1;
            }
            let mut d = fmax;
            while d >= fmin {
                let mut i1 = if self.f(d - 1) >= self.f(d + 1) {
                    self.f(d - 1) + 1
                } else {
                    self.f(d + 1)
                };
                let start = i1;
                let mut i2 = i1 - d;
                while i1 < lim1 && i2 < lim2 && self.eq(i1, i2) {
                    i1 += 1;
                    i2 += 1;
                }
                long_run |= i1 - start > self.limits.snake;
                self.set_f(d, i1);
                if odd && bmin <= d && d <= bmax && self.b(d) <= i1 {
                    return met(i1, i2);
                }
                d -= 2;
            }

            if bmin > dmin {
                bmin -= 1;
                self.set_b(bmin - 1, i64::MAX);
            } else {
                bmin += 1;
            }
            if bmax < dmax {
                bmax += 1;
                self.set_b(bmax + 1, i64::MAX);
            } else {
                bmax -= 1;
            }
            let mut d = bmax;
            while d >= bmin {
                let mut i1 = if self.b(d - 1) < self.b(d + 1) {
                    self.b(d - 1)
                } else {
                    self.b(d + 1) - 1
                };
                let start = i1;
                let mut i2 = i1 - d;
                while i1 > off1 && i2 > off2 && self.eq(i1 - 1, i2 - 1) {
                    i1 -= 1;
                    i2 -= 1;
                }
                long_run |= start - i1 > self.limits.snake;
                self.set_b(d, i1);
                if !odd && fmin <= d && d <= fmax && i1 <= self.f(d) {
                    return met(i1, i2);
                }
                d -= 2;
            }

            if !exact {
                if long_run
                    && cost > self.limits.heuristic_min
                    && let Some(split) = self.promising(
                        (off1, lim1, off2, lim2),
                        (fmin, fmax, fmid),
                        (bmin, bmax, bmid),
                        cost,
                    )
                {
                    return split;
                }
                if cost >= self.limits.max_cost {
                    return self.furthest((off1, lim1, off2, lim2), (fmin, fmax), (bmin, bmax));
                }
            }
            cost += 1;
        }
    }

    /// A diagonal that has run far for its cost and ends in a long match:
    /// good enough to cut the box there without finishing the search.
    fn promising(
        &self,
        (off1, lim1, off2, lim2): (i64, i64, i64, i64),
        (fmin, fmax, fmid): (i64, i64, i64),
        (bmin, bmax, bmid): (i64, i64, i64),
        cost: i64,
    ) -> Option<Split> {
        let snake = self.limits.snake;
        let mut best = 0;
        let mut found = None;
        let mut d = fmax;
        while d >= fmin {
            let i1 = self.f(d);
            let i2 = i1 - d;
            let value = (i1 - off1) + (i2 - off2) - (d - fmid).abs();
            if value > HEURISTIC_FACTOR * cost
                && value > best
                && off1 + snake <= i1
                && i1 < lim1
                && off2 + snake <= i2
                && i2 < lim2
                && (1..=snake).all(|k| self.eq(i1 - k, i2 - k))
            {
                best = value;
                found = Some((i1, i2));
            }
            d -= 2;
        }
        if let Some((i1, i2)) = found {
            return Some(Split {
                i1,
                i2,
                exact_low: true,
                exact_high: false,
            });
        }
        let mut d = bmax;
        while d >= bmin {
            let i1 = self.b(d);
            let i2 = i1 - d;
            let value = (lim1 - i1) + (lim2 - i2) - (d - bmid).abs();
            if value > HEURISTIC_FACTOR * cost
                && value > best
                && off1 < i1
                && i1 <= lim1 - snake
                && off2 < i2
                && i2 <= lim2 - snake
                && (0..snake).all(|k| self.eq(i1 + k, i2 + k))
            {
                best = value;
                found = Some((i1, i2));
            }
            d -= 2;
        }
        found.map(|(i1, i2)| Split {
            i1,
            i2,
            exact_low: false,
            exact_high: true,
        })
    }

    /// The search has cost too much: cut at the point either direction has
    /// carried furthest.
    fn furthest(
        &self,
        (off1, lim1, off2, lim2): (i64, i64, i64, i64),
        (fmin, fmax): (i64, i64),
        (bmin, bmax): (i64, i64),
    ) -> Split {
        let (mut fbest, mut fbest1) = (-1, -1);
        let mut d = fmax;
        while d >= fmin {
            let mut i1 = self.f(d).min(lim1);
            let mut i2 = i1 - d;
            if lim2 < i2 {
                (i1, i2) = (lim2 + d, lim2);
            }
            if fbest < i1 + i2 {
                (fbest, fbest1) = (i1 + i2, i1);
            }
            d -= 2;
        }
        let (mut bbest, mut bbest1) = (i64::MAX, i64::MAX);
        let mut d = bmax;
        while d >= bmin {
            let mut i1 = self.b(d).max(off1);
            let mut i2 = i1 - d;
            if i2 < off2 {
                (i1, i2) = (off2 + d, off2);
            }
            if i1 + i2 < bbest {
                (bbest, bbest1) = (i1 + i2, i1);
            }
            d -= 2;
        }
        if (lim1 + lim2) - bbest < fbest - (off1 + off2) {
            Split {
                i1: fbest1,
                i2: fbest - fbest1,
                exact_low: true,
                exact_high: false,
            }
        } else {
            Split {
                i1: bbest1,
                i2: bbest - bbest1,
                exact_low: false,
                exact_high: true,
            }
        }
    }
}
