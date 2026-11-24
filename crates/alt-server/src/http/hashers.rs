//! The places among those that hash passwords.
//!
//! A password costs memory and a good part of a second to check, so no more are
//! checked at once than there are places. A sign-in that finds them taken waits
//! for one, as long as a request may run, instead of being sent away at once:
//! many people signing in at the same moment are all let in, one after the other.
//! An address has one sign-in waiting at a time, so that whoever sends many of
//! them from one address is not most of those that wait. Whether a sign-in has a
//! place, or the wait of its address, is settled before it is counted among the
//! attempts: one that is sent away for the wait being taken costs its address
//! none of the attempts it may make.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) struct Hashers {
    places: Arc<Semaphore>,
    waiting: Mutex<HashSet<String>>,
}

/// An address among those that wait, until it is let go of.
struct Waiting<'a>(&'a Hashers, &'a str);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.waiting.lock().expect("sign-ins").remove(self.1);
    }
}

/// What a sign-in has before its password is checked: a place, or the wait of
/// its address for one.
pub(super) struct Line<'a> {
    place: Option<OwnedSemaphorePermit>,
    waiting: Option<Waiting<'a>>,
}

impl Line<'_> {
    /// The place, waited for no longer than `wait`; none where none came free.
    pub(super) async fn place(self, wait: Duration) -> Option<OwnedSemaphorePermit> {
        let Some(waiting) = &self.waiting else {
            return self.place;
        };
        let places = waiting.0.places.clone();
        let waited = tokio::time::timeout(wait, places.acquire_owned());
        waited.await.ok()?.ok()
    }
}

impl Hashers {
    pub(super) fn new(places: usize) -> Self {
        Hashers {
            places: Arc::new(Semaphore::new(places)),
            waiting: Mutex::default(),
        }
    }

    /// A place for a sign-in of `caller`, or the wait of its address for one.
    /// None where a sign-in of the caller waits already.
    pub(super) fn line<'a>(&'a self, caller: &'a str) -> Option<Line<'a>> {
        if let Ok(place) = self.places.clone().try_acquire_owned() {
            let (place, waiting) = (Some(place), None);
            return Some(Line { place, waiting });
        }
        let mut waiting = self.waiting.lock().expect("sign-ins");
        waiting.insert(caller.to_owned()).then(|| Line {
            place: None,
            waiting: Some(Waiting(self, caller)),
        })
    }

    /// A place for a sign-in of `caller`, waited for no longer than `wait`. None
    /// where none came free in that time, or where a sign-in of the caller waits
    /// already.
    pub(super) async fn place(&self, caller: &str, wait: Duration) -> Option<OwnedSemaphorePermit> {
        self.line(caller)?.place(wait).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sign_in_waits_for_a_place_and_an_address_has_one_waiting() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let hashers = Arc::new(Hashers::new(2));
            let long = Duration::from_secs(60);
            let first = hashers.place("a", long).await.unwrap();
            let second = hashers.place("a", long).await.unwrap();
            // none comes free: told so when the time to wait is over
            assert!(
                hashers
                    .place("b", Duration::from_millis(50))
                    .await
                    .is_none()
            );
            assert!(hashers.waiting.lock().unwrap().is_empty());
            let wait = |caller: &'static str| {
                let hashers = Arc::clone(&hashers);
                tokio::spawn(async move { hashers.place(caller, long).await })
            };
            // until so many wait, and no longer than a test may stand still
            let waiting = async |many: usize| {
                let began = std::time::Instant::now();
                while hashers.waiting.lock().unwrap().len() < many {
                    assert!(began.elapsed() < long, "{many} do not come to wait");
                    tokio::task::yield_now().await;
                }
            };
            let (c, d) = (wait("c"), wait("d"));
            waiting(2).await;
            // one more of an address that waits is not taken into the wait
            assert!(hashers.place("c", long).await.is_none());
            // a wait that is given up leaves its address free to wait again
            let given_up = wait("e");
            waiting(3).await;
            given_up.abort();
            assert!(given_up.await.is_err());
            assert_eq!(hashers.waiting.lock().unwrap().len(), 2);
            // the places go to those that wait, in the order they came
            drop(first);
            let third = c.await.unwrap().unwrap();
            assert!(!d.is_finished());
            drop(second);
            let fourth = d.await.unwrap().unwrap();
            assert!(hashers.waiting.lock().unwrap().is_empty());
            drop((third, fourth));
            assert!(hashers.place("f", long).await.is_some());
        });
    }
}
