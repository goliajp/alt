//! Whether what the running thread writes is fsynced before its command
//! answers.
//!
//! A command that only stages leaves what it writes to the kernel, as git
//! does by default: the thread that runs it says so here for as long as it
//! runs ([`leave_unsynced`]), and whatever writes on that thread asks
//! ([`unsynced`]) before it fsyncs. `ALT_RELAXED_DURABILITY` in the
//! environment of the process leaves every command's writes so.

use std::cell::Cell;

thread_local! {
    static LEFT: Cell<bool> = const { Cell::new(false) };
}

/// Whether writes made on this thread are left unsynced.
pub fn unsynced() -> bool {
    LEFT.get() || never_synced()
}

/// Whether the process leaves every command's writes unsynced: nothing
/// it writes is promised to outlive a power cut.
pub fn never_synced() -> bool {
    std::env::var_os("ALT_RELAXED_DURABILITY").is_some()
}

/// Puts back what the thread said before [`leave_unsynced`] when dropped.
pub struct Unsynced(bool);

/// Has this thread leave what it writes unsynced, or sync it, until the
/// value returned is dropped.
pub fn leave_unsynced(on: bool) -> Unsynced {
    Unsynced(LEFT.replace(on))
}

impl Drop for Unsynced {
    fn drop(&mut self) {
        LEFT.set(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_leaves_its_writes_unsynced_for_as_long_as_it_says_so() {
        // the variable of the process is not this test's to set
        if std::env::var_os("ALT_RELAXED_DURABILITY").is_some() {
            return;
        }
        assert!(!unsynced());
        let outer = leave_unsynced(true);
        assert!(unsynced());
        // another thread has said nothing
        assert!(!std::thread::spawn(unsynced).join().unwrap());
        let inner = leave_unsynced(false);
        assert!(!unsynced());
        drop(inner);
        assert!(unsynced());
        drop(outer);
        assert!(!unsynced());
    }
}
