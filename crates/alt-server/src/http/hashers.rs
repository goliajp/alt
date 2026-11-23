//! The places among those that hash passwords.
//!
//! A password costs memory and a good part of a second to check, so no more are
//! checked at once than there are places. A sign-in that finds them taken waits
//! for one, as long as a request may run, instead of being sent away at once:
//! many people signing in at the same moment are all let in, one after the other.
//! An address has one sign-in waiting at a time, so that whoever sends many of
//! them from one address is not most of those that wait.

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

impl Hashers {
    pub(super) fn new(places: usize) -> Self {
        Hashers {
            places: Arc::new(Semaphore::new(places)),
            waiting: Mutex::default(),
        }
    }

    /// A place for a sign-in of `caller`, waited for no longer than `wait`. None
    /// where none came free in that time, or where a sign-in of the caller waits
    /// already.
    pub(super) async fn place(&self, caller: &str, wait: Duration) -> Option<OwnedSemaphorePermit> {
        if let Ok(place) = self.places.clone().try_acquire_owned() {
            return Some(place);
        }
        if !self
            .waiting
            .lock()
            .expect("sign-ins")
            .insert(caller.to_owned())
        {
            return None;
        }
        let _waiting = Waiting(self, caller);
        let waited = tokio::time::timeout(wait, self.places.clone().acquire_owned());
        waited.await.ok()?.ok()
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
            let (c, d) = (wait("c"), wait("d"));
            while hashers.waiting.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
            // one more of an address that waits is not taken into the wait
            assert!(hashers.place("c", long).await.is_none());
            // a wait that is given up leaves its address free to wait again
            let given_up = wait("e");
            while hashers.waiting.lock().unwrap().len() < 3 {
                tokio::task::yield_now().await;
            }
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
