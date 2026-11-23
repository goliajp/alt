//! The nonces of signed pushes: issued with the advertisement, good once.

use std::time::{Duration, Instant};

use super::*;

/// How long a nonce is good for. A push is signed over it before its
/// body is sent, and the body of a large one on a slow line is hours in
/// coming.
const LIFE: Duration = Duration::from_secs(24 * 60 * 60);

/// In-memory single-use nonce table. Cap at 1024 active entries; on
/// overflow the oldest is dropped (LRU FIFO). This is a tradeoff:
/// under burst load an honest client whose info/refs nonce ages out
/// before they POST receive-pack gets refused, but the bound makes
/// the table O(1) memory regardless of traffic.
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
    pub(super) queue: std::collections::VecDeque<(String, Instant)>,
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
    /// against any realistic traffic + 1024-entry table). Inserts it
    /// into the table; on overflow the oldest entry is evicted.
    pub(super) fn issue(&self) -> String {
        let nonce = {
            // 16 bytes from the OS entropy pool via /dev/urandom; we
            // avoid a `rand` dep by reading directly. Failure to read
            // is essentially "running on Mars" — fall back to a
            // timestamp-derived value so the path doesn't crash.
            let mut buf = [0u8; 16];
            match std::fs::File::open("/dev/urandom") {
                Ok(mut f) => {
                    use std::io::Read;
                    let _ = f.read_exact(&mut buf);
                }
                Err(_) => {
                    let ms = unix_ms_now().to_le_bytes();
                    buf[..ms.len()].copy_from_slice(&ms);
                }
            }
            let mut hex = String::with_capacity(32);
            for b in buf {
                hex.push_str(&format!("{b:02x}"));
            }
            hex
        };
        let mut g = self.inner.take();
        if g.queue.len() >= 1024
            && let Some((old, _)) = g.queue.pop_front()
        {
            g.set.remove(&old);
        }
        g.queue.push_back((nonce.clone(), Instant::now()));
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
        let at = g.queue.iter().position(|(held, _)| held == nonce);
        let issued = at.and_then(|at| g.queue.remove(at));
        issued.is_some_and(|(_, issued)| now.saturating_duration_since(issued) <= LIFE)
    }

    /// Whether no nonce of the table is good any more: the table may be
    /// let go of.
    pub(super) fn spent(&self) -> bool {
        self.spent_at(Instant::now())
    }

    fn spent_at(&self, now: Instant) -> bool {
        let newest = self.inner.take().queue.back().map(|(_, issued)| *issued);
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
        let n = t.issue();
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

    /// At cap (1024 entries), issuing one more evicts the oldest;
    /// after eviction the evicted value can't be consumed. This
    /// bounds memory regardless of traffic at the cost of refusing
    /// an honest client whose info/refs nonce aged out before they
    /// posted receive-pack.
    #[test]
    fn nonce_table_evicts_oldest_when_full() {
        let t = NonceTable::new();
        let first = t.issue();
        for _ in 1..1024 {
            let _ = t.issue();
        }
        // table is full now; one more issue must evict `first`.
        let _last = t.issue();
        assert!(
            !t.consume(&first),
            "the evicted (oldest) nonce must no longer be consumable"
        );
    }

    /// A nonce is good for a day, and a table whose nonces are all older
    /// or used has nothing to keep it for.
    #[test]
    fn a_nonce_is_good_for_a_day_and_a_table_of_none_that_are_is_spent() {
        let t = NonceTable::new();
        assert!(t.spent());
        let (old, new) = (t.issue(), t.issue());
        let now = Instant::now();
        assert!(!t.spent_at(now + LIFE - Duration::from_secs(60)));
        assert!(t.spent_at(now + LIFE + Duration::from_secs(60)));
        assert!(!t.consume_at(&old, now + LIFE + Duration::from_secs(60)));
        assert!(t.consume_at(&new, now + LIFE - Duration::from_secs(60)));
        assert!(t.spent());
    }
}
