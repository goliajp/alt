//! The workers: threads that take the requests whose heads are read, so many
//! of them at once. A worker whose request waits for its turn to be large
//! steps aside meanwhile: a thread is started to take its place, and the
//! requests that come are served while it waits. Once its turn comes it
//! takes a place again, which the first worker to end a request or to find
//! none gives up; one that is no longer waited for by then ends its request
//! without a place, and its thread ends with it.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use super::dispatch;
use super::http::Server;
use super::mode::ServeMode;

/// how often a request that waits for a place again looks whether it is still
/// waited for
const LOOK: Duration = Duration::from_millis(100);

thread_local! {
    /// whether the worker of this thread has a place: not while it steps aside
    static PLACED: Cell<bool> = const { Cell::new(false) };
}

pub(super) struct Workers {
    server: Arc<Server>,
    mode: Arc<ServeMode>,
    /// the places there are
    most: usize,
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    /// the workers that have a place
    placed: usize,
    /// those that stepped aside and wait for a place again
    back: usize,
    /// the threads there are, with those that stepped aside
    alive: usize,
    /// no worker takes a place anew
    closed: bool,
}

static RUNNING: OnceLock<Arc<Workers>> = OnceLock::new();

impl Workers {
    /// Starts `most` workers of `server`, which serve `mode`.
    pub(super) fn start(server: Arc<Server>, mode: Arc<ServeMode>, most: usize) -> Arc<Workers> {
        let workers = Arc::new(Workers {
            server,
            mode,
            most,
            state: Mutex::default(),
            changed: Condvar::new(),
        });
        let _ = RUNNING.set(Arc::clone(&workers));
        for _ in 0..most {
            let started = workers.started(&mut workers.lock());
            // the server begins with every worker or does not begin
            assert!(started, "a worker could not be started");
        }
        workers
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("workers")
    }

    /// Starts a worker with a place; false where no thread could be started.
    fn started(self: &Arc<Self>, state: &mut State) -> bool {
        let workers = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("worker".into())
            .spawn(move || workers.work());
        if spawned.is_ok() {
            state.placed += 1;
            state.alive += 1;
        }
        spawned.is_ok()
    }

    fn work(self: Arc<Self>) {
        PLACED.set(true);
        loop {
            let request = self.server.recv();
            let found = request.is_ok();
            if let Ok(request) = request {
                dispatch::serve_one(&self.mode, request);
            }
            let mut state = self.lock();
            if !PLACED.get() {
                // its place was taken by another while it stepped aside
                break;
            }
            if (state.closed && !found) || state.placed + state.back > self.most {
                state.placed -= 1;
                break;
            }
        }
        let mut state = self.lock();
        state.alive -= 1;
        self.changed.notify_all();
    }

    /// Has the workers end once no request is left to them, and waits for
    /// them, those that stepped aside among them.
    pub(super) fn close(&self) {
        self.lock().closed = true;
        self.server.close();
        let mut state = self.lock();
        while state.alive > 0 {
            state = self.changed.wait(state).expect("workers");
        }
    }

    /// What a request of this worker's thread gives back while it waits for
    /// its turn: it is waited for until `by` and while `gone` is not set.
    pub(super) fn aside(gone: Arc<AtomicBool>, by: Option<Instant>) -> Box<dyn alt_shares::Aside> {
        let workers = Arc::clone(RUNNING.get().expect("the workers are started"));
        let stepped = false;
        Box::new(Aside {
            workers,
            gone,
            by,
            stepped,
        })
    }
}

struct Aside {
    workers: Arc<Workers>,
    gone: Arc<AtomicBool>,
    by: Option<Instant>,
    /// it gave its place to another
    stepped: bool,
}

impl Aside {
    fn waited_for(&self) -> bool {
        !self.gone.load(Ordering::Relaxed) && self.by.is_none_or(|by| Instant::now() < by)
    }
}

impl alt_shares::Aside for Aside {
    fn step_aside(&mut self) {
        let workers = &self.workers;
        let mut state = workers.lock();
        // a thread without a place has none to give
        if state.closed || !PLACED.get() {
            return;
        }
        // the place goes to the new thread; where none could be started, it is kept
        state.placed -= 1;
        self.stepped = workers.started(&mut state);
        match self.stepped {
            true => PLACED.set(false),
            false => state.placed += 1,
        }
    }

    fn step_back(&mut self) -> bool {
        if !self.stepped {
            return true;
        }
        let workers = &self.workers;
        workers.lock().back += 1;
        // a worker that waits for a request gives up its place
        workers.server.unblock();
        let mut state = workers.lock();
        loop {
            if state.placed < workers.most {
                state.placed += 1;
                state.back -= 1;
                PLACED.set(true);
                return true;
            }
            // every place is taken, so none was given up for it
            if !self.waited_for() {
                state.back -= 1;
                return false;
            }
            let waited = workers.changed.wait_timeout(state, LOOK);
            state = waited.expect("workers").0;
        }
    }
}
