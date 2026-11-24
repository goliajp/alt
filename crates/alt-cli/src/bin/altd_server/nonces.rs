//! The nonces of signed pushes: issued with the advertisement, good once.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use super::*;

/// How long a nonce is good for. A push is signed over it before its
/// body is sent, and the body of a large one on a slow line is hours in
/// coming.
const LIFE: Duration = Duration::from_secs(24 * 60 * 60);

/// the nonces a table holds, and those of them one asker has
const MOST: usize = 1024;
const EACH: usize = 64;

/// In-memory single-use nonce table, of [`MOST`] nonces. One asker, an
/// address as connections are counted for it, has [`EACH`] of them: a
/// nonce more replaces the oldest of its own, so that whoever asks for
/// many drops none that another asked for. Past the cap of the table the
/// oldest of all is dropped, which takes more askers than a quarter of
/// the cap divided among them: an honest client whose nonce went that
/// way is refused and asks again. The bound makes the table O(1) memory
/// regardless of traffic.
///
/// The table of a repository is kept apart from what is open of the
/// repository: a nonce is good after the repository was closed and
/// opened again between the advertisement and the push.
pub(super) struct NonceTable {
    pub(super) inner: Held<NonceInner>,
}

impl Default for NonceTable {
    fn default() -> Self {
        NonceTable::new()
    }
}

pub(super) struct NonceInner {
    pub(super) queue: std::collections::VecDeque<(String, Instant, Option<IpAddr>)>,
    pub(super) set: std::collections::HashSet<String>,
}

impl NonceTable {
    pub(super) fn new() -> Self {
        NonceTable {
            inner: Held::new(NonceInner {
                queue: std::collections::VecDeque::new(),
                set: std::collections::HashSet::new(),
            }),
        }
    }

    /// Issue a fresh 32-char-hex nonce (128 bits — collision negligible
    /// against any realistic traffic + 1024-entry table) to the asker
    /// counted under `from`, and insert it into the table.
    pub(super) fn issue(&self, from: Option<IpAddr>) -> String {
        let mut buf = [0u8; 16];
        alt_os::random::fill(&mut buf);
        let nonce: String = buf.iter().map(|b| format!("{b:02x}")).collect();
        let mut g = self.inner.take();
        let theirs = |held: &(String, Instant, Option<IpAddr>)| held.2 == from;
        let dropped = match g.queue.iter().filter(|held| theirs(held)).count() >= EACH {
            true => g.queue.iter().position(theirs),
            false => (g.queue.len() >= MOST).then_some(0),
        };
        if let Some((old, ..)) = dropped.and_then(|at| g.queue.remove(at)) {
            g.set.remove(&old);
        }
        g.queue.push_back((nonce.clone(), Instant::now(), from));
        g.set.insert(nonce.clone());
        nonce
    }

    /// Atomically check + consume a nonce. Returns true iff it was in
    /// the table at the time of call and is not older than a nonce may
    /// be; subsequent calls return false.
    pub(super) fn consume(&self, nonce: &str) -> bool {
        self.consume_at(nonce, Instant::now())
    }

    fn consume_at(&self, nonce: &str, now: Instant) -> bool {
        let mut g = self.inner.take();
        if !g.set.remove(nonce) {
            return false;
        }
        // also drop it from the queue so the LRU bookkeeping stays
        // accurate; we accept the O(N) here because consumes are
        // rare relative to issues.
        let at = g.queue.iter().position(|(held, ..)| held == nonce);
        let issued = at.and_then(|at| g.queue.remove(at));
        issued.is_some_and(|(_, issued, _)| now.saturating_duration_since(issued) <= LIFE)
    }

    /// Whether no nonce of the table is good any more: the table may be
    /// let go of.
    pub(super) fn spent(&self) -> bool {
        self.spent_at(Instant::now())
    }

    fn spent_at(&self, now: Instant) -> bool {
        let newest = self.inner.take().queue.back().map(|(_, issued, _)| *issued);
        newest.is_none_or(|issued| now.saturating_duration_since(issued) > LIFE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the table issues a nonce that consumes exactly once;
    /// the second consume on the same value returns false. That's
    /// the single-use anti-replay primitive.
    #[test]
    fn nonce_table_consume_is_single_use() {
        let t = NonceTable::new();
        let n = t.issue(None);
        assert_eq!(n.len(), 32, "issued nonce should be 32 hex chars");
        assert!(t.consume(&n), "first consume must succeed");
        assert!(!t.consume(&n), "second consume on the same nonce must fail");
    }

    /// A nonce that was never issued can't be consumed. Stops a
    /// confused-deputy that tries to pass off a guessed value as
    /// if it had been negotiated.
    #[test]
    fn nonce_table_rejects_never_issued_value() {
        let t = NonceTable::new();
        assert!(
            !t.consume("00000000000000000000000000000000"),
            "consuming a never-issued nonce must fail"
        );
    }

    /// An asker has so many nonces in the table: one more drops the oldest
    /// of its own and none of another's, however many it asks for. With
    /// the table full of the nonces of many askers the oldest of all goes.
    #[test]
    fn an_asker_drops_its_own_nonces_and_a_full_table_the_oldest() {
        let t = NonceTable::new();
        let asker = |n: u8| Some(IpAddr::from([198, 51, 100, n]));
        let (theirs, here) = (t.issue(asker(1)), t.issue(None));
        let first = t.issue(asker(2));
        let kept: Vec<String> = (1..EACH).map(|_| t.issue(asker(2))).collect();
        for _ in 0..4 * MOST {
            let _ = t.issue(asker(2));
        }
        assert!(!t.consume(&first));
        assert!(kept.iter().all(|nonce| !t.consume(nonce)));
        assert!(t.consume(&theirs) && t.consume(&here));
        assert_eq!(t.inner.take().queue.len(), EACH);

        let t = NonceTable::new();
        let first = t.issue(asker(0));
        for n in 0..MOST {
            let _ = t.issue(asker((n / (EACH - 1)) as u8 + 1));
        }
        assert_eq!(t.inner.take().queue.len(), MOST);
        assert!(!t.consume(&first));
    }

    /// A nonce is good for a day, and a table whose nonces are all older
    /// or used has nothing to keep it for.
    #[test]
    fn a_nonce_is_good_for_a_day_and_a_table_of_none_that_are_is_spent() {
        let t = NonceTable::new();
        assert!(t.spent());
        let (old, new) = (t.issue(None), t.issue(None));
        let now = Instant::now();
        assert!(!t.spent_at(now + LIFE - Duration::from_secs(60)));
        assert!(t.spent_at(now + LIFE + Duration::from_secs(60)));
        assert!(!t.consume_at(&old, now + LIFE + Duration::from_secs(60)));
        assert!(t.consume_at(&new, now + LIFE - Duration::from_secs(60)));
        assert!(t.spent());
    }
}
