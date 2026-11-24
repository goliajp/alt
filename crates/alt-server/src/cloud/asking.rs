//! When the platform is asked.
//!
//! A lease that nears its end is renewed by the request that finds it so, and
//! that request waits for the platform. While the platform gives no answer,
//! every request would find the same lease and wait the same time, each on a
//! worker. So one request asks at a time for one thing. While the lease is good
//! for what is left of it, the others go on with it, and after an asking that
//! got no answer nobody asks for a while, longer each time. Once the lease is
//! over there is nothing to go on with: a request then asks whatever came
//! before, and where another is asking and the platform answered last time, it
//! waits for that one. What that one was told is what the one that waited goes
//! by: it looks at the lease again where the platform answered, goes on as one
//! whose asking failed where it did not, and asks in neither case.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// the wait after the first asking without an answer; it doubles up to `MOST`
const FIRST: Duration = Duration::from_secs(1);
const MOST: Duration = Duration::from_secs(60);

#[derive(Default)]
struct State {
    /// what is being asked for now: the service, or an account by its credential
    under_way: Vec<Option<String>>,
    unanswered: u32,
    again: Option<Instant>,
}

impl State {
    fn waits(&self, now: Instant) -> bool {
        self.again.is_some_and(|again| now < again)
    }
}

#[derive(Default)]
pub(crate) struct Asking {
    state: Mutex<State>,
    ended: Condvar,
}

/// The right to ask the platform for one thing. Dropped without
/// [`Turn::answered`], the platform is taken to have given no answer.
pub(crate) struct Turn<'a> {
    asking: &'a Asking,
    of: Option<String>,
    over: bool,
}

/// What a request that finds a lease to renew is to do.
pub(crate) enum Asked<'a> {
    /// ask the platform
    Turn(Turn<'a>),
    /// another request asked meanwhile and was answered: look at the lease
    /// again, and go by what is found without asking
    Look,
    /// go on with the lease as it is
    No,
}

impl Asking {
    /// For the service, or for the account of `credential`; `over` is whether
    /// the lease is good for nothing any more.
    pub(crate) fn turn(&self, credential: Option<&str>, over: bool) -> Asked<'_> {
        self.turn_at(credential, over, Instant::now())
    }

    fn turn_at(&self, credential: Option<&str>, over: bool, now: Instant) -> Asked<'_> {
        let mut state = self.state.lock().unwrap();
        let of = credential.map(str::to_owned);
        if state.under_way.contains(&of) {
            // a platform that answered last time is waited for
            if !over || state.unanswered > 0 {
                return Asked::No;
            }
            let asked = |state: &mut State| state.under_way.contains(&of);
            let state = self.ended.wait_while(state, asked).unwrap();
            // the one waited for got no answer: nor would this one
            return match state.unanswered > 0 {
                true => Asked::No,
                false => Asked::Look,
            };
        }
        if !over && state.waits(now) {
            return Asked::No;
        }
        state.under_way.push(of.clone());
        Asked::Turn(Turn {
            asking: self,
            of,
            over: false,
        })
    }
}

impl Turn<'_> {
    /// The platform answered, whatever it said.
    pub(crate) fn answered(mut self) {
        self.end(true, Instant::now());
    }

    /// The platform was not asked after all: what it would have said is as
    /// unknown as before.
    pub(crate) fn unasked(mut self) {
        let mut state = self.asking.state.lock().unwrap();
        state.under_way.retain(|of| *of != self.of);
        self.over = true;
        self.asking.ended.notify_all();
    }

    fn end(&mut self, answered: bool, now: Instant) {
        let mut state = self.asking.state.lock().unwrap();
        state.under_way.retain(|of| *of != self.of);
        self.over = true;
        if answered {
            state.unanswered = 0;
            state.again = None;
        } else {
            let wait = FIRST.saturating_mul(1 << state.unanswered.min(16));
            state.unanswered += 1;
            state.again = Some(now + wait.min(MOST));
        }
        self.asking.ended.notify_all();
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        if !self.over {
            self.end(false, Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn<'a>(
        asking: &'a Asking,
        of: Option<&str>,
        over: bool,
        now: Instant,
    ) -> Option<Turn<'a>> {
        match asking.turn_at(of, over, now) {
            Asked::Turn(turn) => Some(turn),
            Asked::No => None,
            Asked::Look => panic!("waited for another"),
        }
    }

    #[test]
    fn one_asks_for_a_thing_at_a_time_and_nobody_for_a_while_after_no_answer() {
        let asking = Asking::default();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        // one for the service and one for each account, and no second for any
        let mut service = turn(&asking, None, false, at(0)).unwrap();
        let mut account = turn(&asking, Some("a"), false, at(0)).unwrap();
        assert!(turn(&asking, None, false, at(0)).is_none());
        assert!(turn(&asking, Some("a"), false, at(0)).is_none());
        let mut other = turn(&asking, Some("b"), false, at(0)).unwrap();
        // an answer leaves the next free to ask at once
        service.end(true, at(0));
        other.end(true, at(0));
        turn(&asking, None, false, at(0)).unwrap().end(true, at(0));
        // no answer: nobody asks for a second, for anything; then one may,
        // and after each further one for twice as long, a minute at the most
        account.end(false, at(10));
        assert!(turn(&asking, None, false, at(10)).is_none());
        assert!(turn(&asking, Some("b"), false, at(10)).is_none());
        let mut waited = Vec::new();
        let mut now = 10;
        for _ in 0..8 {
            let waits = |second: &u64| asking.state.lock().unwrap().waits(at(*second));
            let free = (now..).find(|second| !waits(second)).unwrap();
            waited.push(free - now);
            now = free;
            let mut next = turn(&asking, Some("a"), false, at(now)).unwrap();
            next.end(false, at(now));
        }
        assert_eq!(waited, [1, 2, 4, 8, 16, 32, 60, 60]);
        // a lease that is over is asked for though nobody else asks, by one
        // at a time, and after no answer the others do not wait for that one
        let mut over = turn(&asking, Some("a"), true, at(now)).unwrap();
        assert!(turn(&asking, Some("a"), true, at(now)).is_none());
        // a turn that is let go of is one without an answer; an answer ends the waiting
        over.end(true, at(now));
        assert_eq!(asking.state.lock().unwrap().unanswered, 0);
        assert!(turn(&asking, Some("b"), false, at(now)).is_some());
        drop(turn(&asking, None, false, at(now)).unwrap());
        assert!(asking.state.lock().unwrap().waits(Instant::now()));
    }

    /// A lease that is over while the platform answers: whoever comes while
    /// another asks waits for that one, and is told to look again.
    #[test]
    fn a_request_whose_lease_is_over_waits_for_the_one_that_asks() {
        let asking = Asking::default();
        let now = Instant::now();
        let first = turn(&asking, None, true, now).unwrap();
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| matches!(asking.turn_at(None, true, now), Asked::Look));
            // the one that waits is parked, or is yet to come: both end with the answer
            std::thread::sleep(Duration::from_millis(50));
            assert!(!waiting.is_finished());
            first.answered();
            assert!(waiting.join().unwrap());
        });
        // and for another thing nobody waits
        let service = turn(&asking, None, true, now).unwrap();
        assert!(turn(&asking, Some("a"), true, now).is_some());
        service.answered();
    }

    /// Sixteen requests find the lease over while one asks, and the platform
    /// gives that one no answer. None of them asks in its turn, one after
    /// the other for as long as the platform takes each time: they are told
    /// to go on as the one they waited for does, and so is whoever comes
    /// after. Where the one that asks is answered they look at the lease
    /// again, all of them, and none has a turn.
    #[test]
    fn those_that_waited_for_one_that_asked_go_by_what_it_was_told_and_do_not_ask() {
        for answered in [false, true] {
            let asking = Asking::default();
            let now = Instant::now();
            let first = turn(&asking, None, true, now).unwrap();
            let told = std::thread::scope(|scope| {
                let wait = || match asking.turn_at(None, true, now) {
                    Asked::Turn(_) => "turn",
                    Asked::Look => "look",
                    Asked::No => "no",
                };
                let waiting: Vec<_> = (0..16).map(|_| scope.spawn(wait)).collect();
                std::thread::sleep(Duration::from_millis(50));
                assert!(waiting.iter().all(|waiting| !waiting.is_finished()));
                match answered {
                    true => first.answered(),
                    false => drop(first),
                }
                let told: Vec<_> = waiting.into_iter().map(|w| w.join().unwrap()).collect();
                told
            });
            let expected = if answered { "look" } else { "no" };
            assert_eq!(told, [expected; 16], "answered: {answered}");
            assert!(asking.state.lock().unwrap().under_way.is_empty());
        }
    }

    /// A turn given up before the platform was asked changes nothing of when
    /// it is asked next.
    #[test]
    fn a_turn_that_did_not_ask_leaves_the_waiting_as_it_was() {
        let asking = Asking::default();
        let now = Instant::now();
        turn(&asking, None, false, now).unwrap().unasked();
        assert_eq!(asking.state.lock().unwrap().unanswered, 0);
        drop(turn(&asking, None, false, now).unwrap());
        let again = asking.state.lock().unwrap().again;
        turn(&asking, Some("a"), true, now).unwrap().unasked();
        let state = asking.state.lock().unwrap();
        assert_eq!((state.unanswered, state.again), (1, again));
    }
}
