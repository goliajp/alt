//! What a daemon holds between its requests, in the hand of one request
//! at a time.
//!
//! A request that panics with it in hand leaves it wherever the panic
//! found it, and nothing tells how far from whole that is. The process
//! ends there, before any other request takes it: to its callers that is
//! a daemon that crashed, which they are made for. A read then runs in
//! its own process, a write is asked again under the same id, of a
//! daemon that opens what is on disk anew.

use std::ops::{Deref, DerefMut};
use std::sync::{Mutex, MutexGuard};

/// The status a process ends with over a panic with what it held in hand.
pub const IN_DOUBT: i32 = 70;

pub struct Held<T>(Mutex<T>);

/// What is held, until this is let go of.
pub struct InHand<'a, T>(MutexGuard<'a, T>);

impl<T> Held<T> {
    pub fn new(value: T) -> Self {
        Held(Mutex::new(value))
    }

    /// Waits for whoever has it in hand to let go of it.
    pub fn take(&self) -> InHand<'_, T> {
        InHand(self.0.lock().expect("no panic lets go of it"))
    }
}

impl<T> Deref for InHand<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for InHand<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> Drop for InHand<'_, T> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("a request panicked with what this process holds in hand: ending");
            std::process::exit(IN_DOUBT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn one_after_the_other_has_it_in_hand() {
        let held = Arc::new(Held::new(0u32));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let held = Arc::clone(&held);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        let mut value = held.take();
                        let seen = *value;
                        std::thread::yield_now();
                        *value = seen + 1;
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(*held.take(), 8000);
    }

    // run in a process of its own by the test below: one whose requests
    // go on after the panic ends with the status of a test that passed
    #[test]
    fn a_panic_with_it_in_hand_ends_the_process() {
        const CHILD: &str = "ALT_HELD_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let held = Arc::new(Held::new(0u32));
            let theirs = Arc::clone(&held);
            let request = std::thread::spawn(move || {
                let mut value = theirs.take();
                *value += 1;
                panic!("with it in hand");
            });
            let _ = request.join();
            // the next request, of a process that is still there
            let next = std::thread::spawn(move || *held.take());
            let _ = next.join();
            return;
        }
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "held::tests::a_panic_with_it_in_hand_ends_the_process",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert_eq!(child.status.code(), Some(IN_DOUBT), "{child:?}");
    }
}
