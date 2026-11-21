//! The threads of a daemon that serve its connections.
//!
//! Work is given to a thread that waits for some, and a thread is started
//! only when more is given than threads wait: one that has served a
//! request keeps its stack and what its allocator holds, where a thread
//! started for each request maps, faults in and unmaps both again. Work
//! never waits for a thread that is busy.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

struct State<T> {
    given: VecDeque<T>,
    /// how many threads wait for work
    waiting: usize,
    closed: bool,
}

struct Shared<T, S> {
    state: Mutex<State<T>>,
    changed: Condvar,
    serve: S,
}

pub struct Workers<T, S> {
    shared: Arc<Shared<T, S>>,
    threads: Vec<JoinHandle<()>>,
}

impl<T, S: Fn(T)> Shared<T, S> {
    fn run(&self) {
        loop {
            let work = {
                let mut state = self.state.lock().expect("workers mutex poisoned");
                state.waiting += 1;
                while state.given.is_empty() && !state.closed {
                    state = self.changed.wait(state).expect("workers mutex poisoned");
                }
                state.waiting -= 1;
                match state.given.pop_front() {
                    Some(work) => work,
                    None => return,
                }
            };
            (self.serve)(work);
        }
    }
}

impl<T: Send + 'static, S: Fn(T) + Send + Sync + 'static> Workers<T, S> {
    /// Threads that run `serve` on what they are given.
    pub fn new(serve: S) -> Self {
        let state = State {
            given: VecDeque::new(),
            waiting: 0,
            closed: false,
        };
        Workers {
            shared: Arc::new(Shared {
                state: Mutex::new(state),
                changed: Condvar::new(),
                serve,
            }),
            threads: Vec::new(),
        }
    }

    /// Gives `work` to a thread that waits, or to a new one.
    pub fn give(&mut self, work: T) {
        let mut state = self.shared.state.lock().expect("workers mutex poisoned");
        state.given.push_back(work);
        if state.given.len() <= state.waiting {
            drop(state);
            self.shared.changed.notify_one();
            return;
        }
        drop(state);
        let shared = Arc::clone(&self.shared);
        self.threads.push(std::thread::spawn(move || shared.run()));
    }

    /// How many threads were started.
    pub fn started(&self) -> usize {
        self.threads.len()
    }

    /// Waits until the threads have served what they were given and ended.
    pub fn finish(self) {
        self.shared
            .state
            .lock()
            .expect("workers mutex poisoned")
            .closed = true;
        self.shared.changed.notify_all();
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread::ThreadId;

    type Work = (mpsc::Receiver<()>, mpsc::Sender<ThreadId>);

    /// Work that reports the thread it runs on once it is told to end.
    fn work() -> (Work, mpsc::Sender<()>, mpsc::Receiver<ThreadId>) {
        let (end, ended) = mpsc::channel();
        let (ran, on) = mpsc::channel();
        ((ended, ran), end, on)
    }

    fn workers() -> Workers<Work, impl Fn(Work) + Send + Sync + 'static> {
        Workers::new(|(ended, ran): Work| {
            ended.recv().unwrap();
            ran.send(std::thread::current().id()).unwrap();
        })
    }

    fn waiting<S>(workers: &Workers<Work, S>) -> usize {
        workers.shared.state.lock().unwrap().waiting
    }

    #[test]
    fn work_given_one_after_the_other_runs_on_the_same_thread() {
        let mut workers = workers();
        let mut threads = Vec::new();
        for _ in 0..5 {
            let (work, end, on) = work();
            workers.give(work);
            end.send(()).unwrap();
            threads.push(on.recv().unwrap());
            while waiting(&workers) == 0 {
                std::thread::yield_now();
            }
        }
        assert!(threads.iter().all(|thread| *thread == threads[0]));
        assert_eq!(workers.started(), 1);
        workers.finish();
    }

    #[test]
    fn work_does_not_wait_for_a_thread_that_is_busy() {
        let mut workers = workers();
        let (slow, end_slow, slow_on) = work();
        workers.give(slow);
        // the first is still running: the second ends before it
        let (quick, end_quick, quick_on) = work();
        workers.give(quick);
        end_quick.send(()).unwrap();
        let second = quick_on.recv().unwrap();
        end_slow.send(()).unwrap();
        assert_ne!(slow_on.recv().unwrap(), second);
        assert_eq!(workers.started(), 2);
        workers.finish();
    }

    #[test]
    fn finishing_waits_for_work_that_was_given() {
        let mut workers = workers();
        let (first, end_first, first_on) = work();
        let (second, end_second, second_on) = work();
        workers.give(first);
        workers.give(second);
        end_first.send(()).unwrap();
        end_second.send(()).unwrap();
        workers.finish();
        assert!(first_on.try_recv().is_ok() && second_on.try_recv().is_ok());
    }
}
