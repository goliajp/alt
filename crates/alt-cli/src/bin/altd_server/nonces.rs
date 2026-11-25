//! The nonces of signed pushes: given with the advertisement, good once.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use super::*;

/// How long a nonce is good for. A push is signed over it before its
/// body is sent, and the body of a large one on a slow line is hours in
/// coming.
const LIFE: Duration = Duration::from_secs(24 * 60 * 60);

/// The nonces kept as used at once: those of the signed pushes of a day.
/// A push more is refused, not taken with a nonce forgotten.
const MOST: usize = 1 << 20;

/// What tells a nonce from the others of its table: when it was given, in
/// milliseconds since the table was made, and eight bytes of chance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Given(u64, [u8; 8]);

/// The nonces of one repository. A nonce says when it was given and is
/// sealed with a key only this table has, so that giving one keeps
/// nothing: whoever asks for many drops none that another was given. What
/// is kept is the nonces that were used, for as long as they would still
/// be good, and one is used when a signature over it verified: asking for
/// advertisements and sending pushes that do not verify fills nothing.
///
/// The table of a repository is kept apart from what is open of the
/// repository: a nonce is good after the repository was closed and
/// opened again between the advertisement and the push.
pub(super) struct NonceTable {
    key: [u8; 32],
    born: Instant,
    inner: Held<NonceInner>,
}

impl Default for NonceTable {
    fn default() -> Self {
        NonceTable::new()
    }
}

#[derive(Default)]
struct NonceInner {
    used: BTreeSet<Given>,
    /// when the last nonce was given, as nonces say it
    last: Option<u64>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl NonceTable {
    pub(super) fn new() -> Self {
        let mut key = [0u8; 32];
        alt_os::random::fill(&mut key);
        NonceTable {
            key,
            born: Instant::now(),
            inner: Held::new(NonceInner::default()),
        }
    }

    fn at(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.born).as_millis() as u64
    }

    fn seal(&self, given: &Given) -> [u8; 16] {
        let said = [&given.0.to_be_bytes()[..], &given.1].concat();
        let sealed = alt_hash::blake3::keyed_hash(&self.key, &said);
        sealed.as_bytes()[..16].try_into().expect("sixteen of them")
    }

    /// A nonce of sixty-four hex digits that was not given before.
    pub(super) fn issue(&self) -> String {
        let mut chance = [0u8; 8];
        alt_os::random::fill(&mut chance);
        let given = Given(self.at(Instant::now()), chance);
        self.inner.take().last = Some(given.0);
        let seal = self.seal(&given);
        hex(&[&given.0.to_be_bytes()[..], &given.1, &seal].concat())
    }

    /// What `nonce` is, where this table gave it, it is no older than a
    /// nonce may be and it was not used.
    pub(super) fn good(&self, nonce: &str) -> Option<Given> {
        self.good_at(nonce, Instant::now())
    }

    fn good_at(&self, nonce: &str, now: Instant) -> Option<Given> {
        // as it was given: no sign before a digit, and no capital
        let given = |digit: u8| digit.is_ascii_digit() || (b'a'..=b'f').contains(&digit);
        if nonce.len() != 64 || !nonce.bytes().all(given) {
            return None;
        }
        let digit = |at: usize| u8::from_str_radix(&nonce[at..at + 2], 16).ok();
        let bytes: Vec<u8> = (0..64).step_by(2).map(digit).collect::<Option<_>>()?;
        let given = Given(
            u64::from_be_bytes(bytes[..8].try_into().expect("eight of them")),
            bytes[8..16].try_into().expect("eight of them"),
        );
        // every byte is looked at, whichever is the first that differs
        let differ =
            (self.seal(&given).iter().zip(&bytes[16..])).fold(0, |all, (a, b)| all | (a ^ b));
        let age = self.at(now).checked_sub(given.0)?;
        let good = differ == 0 && u128::from(age) <= LIFE.as_millis();
        (good && !self.inner.take().used.contains(&given)).then_some(given)
    }

    /// Has the nonce `given` be used from now on. False where it was used
    /// meanwhile, or the table keeps as many as it may: the push that is
    /// signed over it is not taken.
    pub(super) fn spend(&self, given: Given) -> bool {
        self.spend_at(given, Instant::now())
    }

    fn spend_at(&self, given: Given, now: Instant) -> bool {
        let mut inner = self.inner.take();
        // those that are good no longer are told from the used by their age
        let oldest = self.at(now).saturating_sub(LIFE.as_millis() as u64);
        inner.used = inner.used.split_off(&Given(oldest, [0; 8]));
        if inner.used.len() >= MOST {
            eprintln!(
                "altd-server: a signed push is refused: the {MOST} nonces used within a day are kept, and no more"
            );
            return false;
        }
        inner.used.insert(given)
    }

    /// Whether no nonce of the table is good any more: the table may be
    /// let go of.
    pub(super) fn spent(&self) -> bool {
        self.spent_at(Instant::now())
    }

    fn spent_at(&self, now: Instant) -> bool {
        let last = self.inner.take().last;
        last.is_none_or(|last| u128::from(self.at(now) - last) > LIFE.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonce_is_good_until_it_is_used_and_not_again() {
        let t = NonceTable::new();
        let n = t.issue();
        assert_eq!(n.len(), 64);
        let given = t.good(&n).unwrap();
        // good for as long as no signature over it verified
        assert_eq!(t.good(&n), Some(given));
        assert!(t.spend(given));
        assert_eq!(t.good(&n), None);
        // two pushes that found it good: one of them has it
        assert!(!t.spend(given));
    }

    /// What the table did not give is no nonce of it: a guess, one of
    /// another table, and one of its own with a digit changed, in its time,
    /// its chance or its seal.
    #[test]
    fn a_nonce_the_table_did_not_give_is_not_good() {
        let (t, other) = (NonceTable::new(), NonceTable::new());
        assert_eq!(t.good(&"0".repeat(64)), None);
        assert_eq!(t.good(&other.issue()), None);
        let n = t.issue();
        for at in [0, 15, 16, 31, 32, 63] {
            let mut changed = n.clone().into_bytes();
            changed[at] = if changed[at] == b'0' { b'1' } else { b'0' };
            assert_eq!(t.good(&String::from_utf8(changed).unwrap()), None, "{at}");
        }
        for odd in [
            "",
            &n[..63],
            &format!("{n}0"),
            &n.to_uppercase().replace('0', "g"),
        ] {
            assert_eq!(t.good(odd), None, "{odd}");
        }
        assert!(t.good(&n).is_some());
    }

    /// Giving a nonce keeps nothing. One given before a hundred thousand
    /// others, to whomever, is as good as it was, and so is every one of
    /// them.
    #[test]
    fn however_many_nonces_are_asked_for_one_given_before_them_is_good() {
        let t = NonceTable::new();
        let first = t.issue();
        let later: Vec<String> = (0..100_000).map(|_| t.issue()).collect();
        assert!(t.good(&first).is_some());
        assert!(later.iter().all(|nonce| t.good(nonce).is_some()));
        assert!(t.inner.take().used.is_empty());
    }

    /// A nonce is good for a day. One that was used is kept for as long, and
    /// a table that gave none for a day has nothing to be kept for.
    #[test]
    fn a_nonce_is_good_for_a_day_and_a_used_one_is_kept_as_long() {
        let t = NonceTable::new();
        assert!(t.spent());
        let (old, new) = (t.issue(), t.issue());
        let now = Instant::now();
        let (before, after) = (
            now + LIFE - Duration::from_secs(60),
            now + LIFE + Duration::from_secs(60),
        );
        assert!(!t.spent_at(before) && t.spent_at(after));
        assert!(t.good_at(&old, after).is_none());
        // good as it was given, and not written another way that reads as the same digits
        let other = [new.to_uppercase(), new.replacen('0', "+", 1)];
        assert!(other.iter().all(|other| *other != new && other.len() == 64));
        assert!(other.iter().all(|other| t.good_at(other, before).is_none()));
        let given = t.good_at(&new, before).unwrap();
        assert!(t.spend_at(given, before));
        assert_eq!(t.inner.take().used.len(), 1);
        // past its day it is told by its age, and kept no more
        let other = Given(t.at(after), [7; 8]);
        assert!(t.spend_at(other, after));
        assert_eq!(t.inner.take().used.iter().collect::<Vec<_>>(), [&other]);
    }

    /// The used nonces of a day are kept, as many as the table may: with
    /// that many a push more is refused, and none of them is forgotten.
    #[test]
    fn a_table_that_keeps_as_many_used_nonces_as_it_may_takes_no_more() {
        let t = NonceTable::new();
        let now = Instant::now();
        t.inner.take().used = (0..MOST as u64)
            .map(|n| Given(1, n.to_be_bytes()))
            .collect();
        let more = Given(2, [0xff; 8]);
        assert!(!t.spend_at(more, now + Duration::from_secs(1)));
        assert_eq!(t.inner.take().used.len(), MOST);
        // a day on, those are no longer good and there is room
        assert!(t.spend_at(more, now + LIFE + Duration::from_secs(1)));
        assert_eq!(t.inner.take().used.len(), 1);
    }
}
