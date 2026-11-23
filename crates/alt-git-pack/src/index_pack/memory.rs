//! The memory that packs being indexed at once may take between them.
//!
//! One pack has its largest object in memory three times over while it is
//! resolved: as the base of a delta, as the instructions of the delta and
//! as what they make. A server that indexes in its own process, a pack to
//! a worker, would take that times its workers at the worst, and end for
//! all of them when the allocation fails. Here each pack takes what it
//! needs out of one amount before it allocates, and waits while others
//! have it.

use std::sync::{Condvar, Mutex};

use crate::PackError;

#[derive(Debug)]
pub struct Memory {
    all: u64,
    free: Mutex<u64>,
    given_back: Condvar,
}

/// A part of a [`Memory`], given back when it is dropped.
#[derive(Debug)]
pub struct Taken<'a> {
    of: &'a Memory,
    bytes: u64,
}

impl Memory {
    pub const fn new(bytes: u64) -> Self {
        Memory {
            all: bytes,
            free: Mutex::new(bytes),
            given_back: Condvar::new(),
        }
    }

    /// Takes `bytes`, once the packs that have them are done. A pack that
    /// needs more than there is at all is refused.
    pub fn take(&self, bytes: u64) -> Result<Taken<'_>, PackError> {
        if bytes > self.all {
            return Err(PackError::Limit("index memory"));
        }
        let mut free = self.free.lock().expect("index memory");
        while *free < bytes {
            free = self.given_back.wait(free).expect("index memory");
        }
        *free -= bytes;
        Ok(Taken { of: self, bytes })
    }
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        *self.of.free.lock().expect("index memory") += self.bytes;
        self.of.given_back.notify_all();
    }
}
