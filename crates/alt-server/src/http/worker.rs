//! The worker a request holds, given back while its response waits for its
//! turn to be large: those that wait for a turn take no worker from the
//! requests that would be served meanwhile. A response put off to wait for
//! its turn is built again once it has it, in a time of its own.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

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
    /// told each time a response of the request has its turn after it was put off
    turned: Arc<Notify>,
}

impl Slot {
    /// `permit`, one of `workers`; called on the runtime of the server
    pub(crate) fn new(permit: OwnedSemaphorePermit, workers: &Arc<Semaphore>) -> Self {
        Slot {
            held: Arc::new(Mutex::new(Some(permit))),
            workers: Arc::clone(workers),
            runtime: tokio::runtime::Handle::current(),
            turned: Arc::new(Notify::new()),
        }
    }

    /// What `work` gives, where it is done within `time`, and within `time`
    /// again from each time a response of the request has its turn after it
    /// was put off: what is built again then is not timed by what it took to
    /// be built before and to wait.
    pub(crate) async fn within<F: Future>(&self, time: Duration, work: F) -> Option<F::Output> {
        let mut work = std::pin::pin!(work);
        loop {
            let turned = self.turned.notified();
            let done = async {
                tokio::select! {
                    done = &mut work => Some(done),
                    () = turned => None,
                }
            };
            match tokio::time::timeout(time, done).await {
                Ok(Some(done)) => return Some(done),
                Ok(None) => {}
                Err(_) => return None,
            }
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

    fn turned(&mut self) {
        self.slot.turned.notify_one();
    }
}
