//! The worker a request holds, given back while its response waits for its
//! turn to be large: those that wait for a turn take no worker from the
//! requests that would be served meanwhile.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// how often a request that waits for a worker again looks whether it is
/// still waited for
const LOOK: Duration = Duration::from_millis(100);

/// The worker of one request: held from the start, and given back for a time
/// where the request waits.
#[derive(Clone)]
pub(crate) struct Slot {
    held: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
    workers: Arc<Semaphore>,
    runtime: tokio::runtime::Handle,
}

impl Slot {
    /// `permit`, one of `workers`; called on the runtime of the server
    pub(crate) fn new(permit: OwnedSemaphorePermit, workers: &Arc<Semaphore>) -> Self {
        Slot {
            held: Arc::new(Mutex::new(Some(permit))),
            workers: Arc::clone(workers),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// What gives the worker back while a response of a request whose answer
    /// is waited for until `gone` is set waits for its turn.
    pub(crate) fn aside(&self, gone: Arc<AtomicBool>) -> Box<dyn alt_shares::Aside> {
        Box::new(Aside {
            slot: self.clone(),
            gone,
        })
    }

    /// Holds the worker until the work ends: whatever of it is then held is
    /// given back, though a response that is sent later keeps its share.
    pub(crate) fn until_done(&self) -> Done {
        Done(Arc::clone(&self.held))
    }
}

/// gives the worker of a request back when the work of the request ends
pub(crate) struct Done(Arc<Mutex<Option<OwnedSemaphorePermit>>>);

impl Drop for Done {
    fn drop(&mut self) {
        drop(self.0.lock().expect("worker").take());
    }
}

struct Aside {
    slot: Slot,
    gone: Arc<AtomicBool>,
}

impl alt_shares::Aside for Aside {
    fn step_aside(&mut self) {
        drop(self.slot.held.lock().expect("worker").take());
    }

    fn step_back(&mut self) -> bool {
        let workers = &self.slot.workers;
        while !self.gone.load(Relaxed) {
            let waited = tokio::time::timeout(LOOK, Arc::clone(workers).acquire_owned());
            match self.slot.runtime.block_on(waited) {
                Ok(Ok(permit)) => {
                    *self.slot.held.lock().expect("worker") = Some(permit);
                    return true;
                }
                Ok(Err(_)) => return false,
                Err(_) => {}
            }
        }
        false
    }
}
