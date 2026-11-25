//! Whether this process serves its database just now, and what of it is in
//! use: a process that may have lost its lock on the database serves nothing
//! new until it has it again, and lets what is in use end first.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

/// The way into the database of the process: open, or closed while the lock
/// of the database is taken again.
#[derive(Default)]
pub struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    closed: bool,
    /// the stores in use
    inside: usize,
}

/// A store in use, let out of the gate when it is dropped.
pub struct Inside(Arc<Gate>);

/// what a store is refused with while the gate is closed
pub const CLOSED: &str = "the server takes its lock of the database again";

impl Gate {
    /// A store in use from now on; none while the gate is closed.
    pub fn enter(self: &Arc<Self>) -> alt_server_store::Result<Inside> {
        let mut state = self.state.lock().expect("gate");
        if state.closed {
            return Err(alt_server_store::Error::Busy(CLOSED));
        }
        state.inside += 1;
        Ok(Inside(Arc::clone(self)))
    }

    /// Closes the gate, and waits until no store is in use or until `by`.
    /// Whether none is.
    pub fn close(&self, by: Instant) -> bool {
        let mut state = self.state.lock().expect("gate");
        state.closed = true;
        while state.inside > 0 {
            let left = by.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self.changed.wait_timeout(state, left).expect("gate").0;
        }
        true
    }

    pub fn open(&self) {
        self.state.lock().expect("gate").closed = false;
    }

    /// whether the gate is closed
    pub fn closed(&self) -> bool {
        self.state.lock().expect("gate").closed
    }
}

impl Drop for Inside {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("gate");
        state.inside -= 1;
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_closed_gate_lets_nothing_in_and_waits_for_what_is_inside() {
        let gate = Arc::new(Gate::default());
        let inside = gate.enter().unwrap();
        // what is inside is waited for until the time is over
        let by = Instant::now() + Duration::from_millis(50);
        assert!(!gate.close(by));
        assert!(Instant::now() >= by);
        assert!(gate.enter().is_err());
        let leaving = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(inside);
        });
        assert!(gate.close(Instant::now() + Duration::from_secs(10)));
        leaving.join().unwrap();
        gate.open();
        assert!(gate.enter().is_ok());
    }
}
