//! When the platform is asked.
//!
//! A lease that nears its end is renewed by the request that finds it so, and
//! that request waits for the platform. While the platform gives no answer,
//! every request would find the same lease and wait the same time, each on a
//! worker. So one request asks at a time for one thing, and after an asking
//! that got no answer nobody asks for a while, longer each time: the others go
//! on with what the lease still allows, as they do when the asking fails.

use std::sync::Mutex;
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

#[derive(Default)]
pub(crate) struct Asking(Mutex<State>);

/// The right to ask the platform for one thing. Dropped without
/// [`Turn::answered`], the platform is taken to have given no answer.
pub(crate) struct Turn<'a> {
    asking: &'a Asking,
    of: Option<String>,
    over: bool,
}

impl State {
    fn waits(&self, now: Instant) -> bool {
        self.again.is_some_and(|again| now < again)
    }
}

impl Asking {
    /// A turn to ask for the service, or for the account of `credential`; none
    /// while another request asks for the same, and none for a time after the
    /// platform gave no answer.
    pub(crate) fn turn(&self, credential: Option<&str>) -> Option<Turn<'_>> {
        self.turn_at(credential, Instant::now())
    }

    fn turn_at(&self, credential: Option<&str>, now: Instant) -> Option<Turn<'_>> {
        let mut state = self.0.lock().unwrap();
        let of = credential.map(str::to_owned);
        if state.waits(now) || state.under_way.contains(&of) {
            return None;
        }
        state.under_way.push(of.clone());
        Some(Turn {
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

    fn end(&mut self, answered: bool, now: Instant) {
        let mut state = self.asking.0.lock().unwrap();
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

    #[test]
    fn one_asks_for_a_thing_at_a_time_and_nobody_for_a_while_after_no_answer() {
        let asking = Asking::default();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        // one for the service and one for each account, and no second for any
        let mut service = asking.turn_at(None, at(0)).unwrap();
        let mut account = asking.turn_at(Some("a"), at(0)).unwrap();
        assert!(asking.turn_at(None, at(0)).is_none());
        assert!(asking.turn_at(Some("a"), at(0)).is_none());
        let mut other = asking.turn_at(Some("b"), at(0)).unwrap();
        // an answer leaves the next free to ask at once
        service.end(true, at(0));
        other.end(true, at(0));
        asking.turn_at(None, at(0)).unwrap().end(true, at(0));
        // no answer: nobody asks for a second, for anything; then one may,
        // and after each further one for twice as long, a minute at the most
        account.end(false, at(10));
        assert!(asking.turn_at(None, at(10)).is_none());
        assert!(asking.turn_at(Some("b"), at(10)).is_none());
        let mut waited = Vec::new();
        let mut now = 10;
        for _ in 0..8 {
            let free = (now..).find(|second| !asking.0.lock().unwrap().waits(at(*second)));
            let free = free.unwrap();
            waited.push(free - now);
            now = free;
            asking
                .turn_at(Some("a"), at(now))
                .unwrap()
                .end(false, at(now));
        }
        assert_eq!(waited, [1, 2, 4, 8, 16, 32, 60, 60]);
        // a turn that is let go of is one without an answer; an answer ends the waiting
        now += 60;
        drop(asking.turn_at(None, at(now)).unwrap());
        assert!(asking.0.lock().unwrap().waits(Instant::now()));
        asking.0.lock().unwrap().again = None;
        asking.turn_at(None, at(now)).unwrap().end(true, at(now));
        assert_eq!(asking.0.lock().unwrap().unanswered, 0);
        assert!(asking.turn_at(Some("a"), at(now)).is_some());
    }
}
