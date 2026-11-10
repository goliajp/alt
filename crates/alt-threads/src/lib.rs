//! A pool of worker threads for work that splits in two.
//!
//! [`join`] runs two closures, the second one on another thread when one is free. The
//! threads are started once and kept, so a split costs a queue operation, not a thread.

use std::{
    any::Any,
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};

const WAITING: u8 = 0;
const TAKEN: u8 = 1;
const DONE: u8 = 2;

/// The second half of a `join`, offered to the pool. The closure and the place for
/// its result live on the stack of the thread that called `join`, which does not
/// return before the job is done or it has taken the job back.
struct Job {
    state: AtomicU8,
    run: unsafe fn(*mut ()),
    data: *mut (),
    /// held while `state` changes to `DONE`, so that the waiter cannot miss it
    finished: Mutex<()>,
    wake: Condvar,
}

// the pointer is only used by whoever moved `state` from WAITING to TAKEN, while the
// owner of what it points to is blocked in `join`
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

struct Pool {
    queue: Mutex<VecDeque<Arc<Job>>>,
    /// how many jobs the queue holds, readable without its lock
    queued: AtomicUsize,
    work: Condvar,
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let pool: &'static Pool = Box::leak(Box::new(Pool {
            queue: Mutex::new(VecDeque::new()),
            queued: AtomicUsize::new(0),
            work: Condvar::new(),
        }));
        // the calling thread works too, so one fewer than there are processors
        let workers = std::thread::available_parallelism().map_or(0, |count| count.get() - 1);
        for index in 0..workers {
            std::thread::Builder::new()
                .name(format!("alt-worker-{index}"))
                .spawn(move || pool.work())
                .expect("starting a worker thread");
        }
        pool
    })
}

/// how often an idle thread looks at the queue again before it goes to sleep: waking
/// a sleeping thread takes longer than most jobs do
const SPINS: u32 = 2_000;

impl Pool {
    /// the oldest job, which is the largest piece of work still unclaimed
    fn take(&self) -> Option<Arc<Job>> {
        // idle threads ask this in a loop: only touch the lock when there is something
        if self.queued.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let job = self.queue.lock().unwrap().pop_front();
        if job.is_some() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
        }
        job
    }

    fn work(&self) -> ! {
        loop {
            let mut idle = 0;
            while idle < SPINS {
                match self.take() {
                    Some(job) => {
                        job.attempt();
                        idle = 0;
                    }
                    None => {
                        std::hint::spin_loop();
                        idle += 1;
                    }
                }
            }
            let mut queue = self.queue.lock().unwrap();
            while queue.is_empty() {
                queue = self.work.wait(queue).unwrap();
            }
        }
    }
}

impl Job {
    /// runs the job unless someone else already took it
    fn attempt(&self) {
        if self
            .state
            .compare_exchange(WAITING, TAKEN, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // this thread won the job, and its owner is waiting for DONE
        unsafe { (self.run)(self.data) };
        let guard = self.finished.lock().unwrap();
        self.state.store(DONE, Ordering::Release);
        drop(guard);
        self.wake.notify_one();
    }
}

type Panic = Box<dyn Any + Send>;

/// what the second closure of a `join` turns into while it waits to be run
struct Half<B, R> {
    closure: Option<B>,
    result: Option<Result<R, Panic>>,
}

unsafe fn run_half<B: FnOnce() -> R, R>(data: *mut ()) {
    // `data` is the `Half` on the stack of the thread blocked in `join`
    let half = unsafe { &mut *data.cast::<Half<B, R>>() };
    let closure = half.closure.take().unwrap();
    half.result = Some(catch_unwind(AssertUnwindSafe(closure)));
}

/// Runs both closures and returns both results. `second` runs on a pool thread if one
/// picks it up before `first` is finished, otherwise on this thread after it. A panic
/// in either closure is raised again here, after both have ended.
pub fn join<A, B, RA, RB>(first: A, second: B) -> (RA, RB)
where
    A: FnOnce() -> RA,
    B: FnOnce() -> RB + Send,
    RB: Send,
{
    let pool = pool();
    let mut half = Half {
        closure: Some(second),
        result: None,
    };
    let job = Arc::new(Job {
        state: AtomicU8::new(WAITING),
        run: run_half::<B, RB>,
        data: (&raw mut half).cast(),
        finished: Mutex::new(()),
        wake: Condvar::new(),
    });
    pool.queue.lock().unwrap().push_back(job.clone());
    pool.queued.fetch_add(1, Ordering::Relaxed);
    pool.work.notify_one();

    let first = catch_unwind(AssertUnwindSafe(first));
    // take the job back if no worker has started it; the queue's copy then finds it taken
    job.attempt();
    // a worker has it: do other queued work meanwhile, then wait for a moment, then sleep
    let mut idle = 0;
    while job.state.load(Ordering::Acquire) != DONE && idle < SPINS {
        match pool.take() {
            Some(other) => {
                other.attempt();
                idle = 0;
            }
            None => {
                std::hint::spin_loop();
                idle += 1;
            }
        }
    }
    let mut guard = job.finished.lock().unwrap();
    while job.state.load(Ordering::Acquire) != DONE {
        guard = job.wake.wait(guard).unwrap();
    }
    drop(guard);
    // DONE: nobody touches `half` through the job any more
    let second = half.result.take().unwrap();
    match (first, second) {
        (Ok(first), Ok(second)) => (first, second),
        (Err(panic), _) | (_, Err(panic)) => resume_unwind(panic),
    }
}
