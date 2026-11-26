//! The session by which this process is the one that serves its database,
//! looked at every so often for as long as it serves.
//!
//! The lock lives as long as the session. The session is gone where asking it
//! fails, as it does once the database ended it or the connection broke, and
//! where the database tells another session that no session, or another one,
//! holds the lock:
//! the process stops serving (the gate of its stores shuts, and what is in use
//! is let end, for as long as a request may take), ends the session, and
//! begins another and asks for the lock again. It serves again once it has
//! it; where another process took it meanwhile, this one serves no longer.
//!
//! A session that is slow to answer is not gone: the database tells another
//! session that it still holds the lock, and the process serves on and waits
//! for the answer. Where the database is not reached at all nothing is known,
//! and the process serves on as well: what it serves needs the database, and
//! the lock is the session's until the database ends it, which another session
//! is then told.

use super::StoreFactory;
use alt_server_store::{Error, SERVED, Serving, Store};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// how long a process that begins waits for the one before it to be gone
const WAIT: Duration = Duration::from_secs(2);
/// How long a process whose session was lost waits for the lock: as long as
/// the database takes to end a session it hears nothing of, which may be the
/// one that was lost, and a little more.
const AGAIN: Duration = alt_server_store::UNHEARD.saturating_add(Duration::from_secs(5));
/// how often the session is looked at
const EVERY: Duration = Duration::from_secs(5);
/// how long an answer of the session is waited for before the database is
/// asked who holds the lock, and how often after that
const SLOW: Duration = Duration::from_secs(10);
const LOOK: Duration = Duration::from_secs(1);

pub(super) struct Alone {
    /// dropped when the process serves no longer: the watch ends
    _serving: mpsc::Sender<()>,
    watch: std::thread::JoinHandle<()>,
    /// told when another process has the lock
    pub(super) lost: tokio::sync::oneshot::Receiver<()>,
}

fn begun(factory: &StoreFactory, wait: Duration) -> alt_server_store::Result<Serving> {
    factory.connect()?.serve_alone(wait)
}

/// the session that holds the lock, as another session is told
fn holder(factory: &StoreFactory) -> alt_server_store::Result<Option<i32>> {
    factory.connect().and_then(Store::serving_session)
}

/// what the session that held the lock is found to be
enum Found {
    Held,
    /// it is gone, or another session holds the lock
    Gone,
    /// the process serves no longer
    Ended,
}

/// The session of the lock, asked on a thread of its own whether it is there,
/// so that an answer it is slow with is waited for while the database is
/// asked about it on another session. Dropped, it ends the connection.
struct Session {
    process: i32,
    ask: mpsc::Sender<()>,
    told: mpsc::Receiver<alt_server_store::Result<()>>,
    closer: alt_pg::Closer,
}

impl Session {
    fn new(mut serving: Serving) -> alt_server_store::Result<Self> {
        let (process, closer) = (serving.session(), serving.closer()?);
        let (ask, asked) = mpsc::channel::<()>();
        let (tell, told) = mpsc::channel();
        std::thread::spawn(move || {
            while asked.recv().is_ok() {
                if tell.send(serving.held()).is_err() {
                    return;
                }
            }
        });
        Ok(Session {
            process,
            ask,
            told,
            closer,
        })
    }

    /// Whether the session is there, its answer waited for while another
    /// session tells it holds the lock, or nothing is told, and while the
    /// process serves.
    fn found(&self, factory: &StoreFactory, serves: &mpsc::Receiver<()>) -> Found {
        if self.ask.send(()).is_err() {
            return Found::Gone;
        }
        let mut wait = SLOW;
        loop {
            match self.told.recv_timeout(wait) {
                Ok(Ok(())) => return Found::Held,
                Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => return Found::Gone,
                Err(RecvTimeoutError::Timeout) => {}
            }
            match holder(factory) {
                Ok(Some(process)) if process == self.process => {}
                Ok(_) => return Found::Gone,
                // the database is not reached: nothing is known of the session
                Err(_) => {}
            }
            if serves.try_recv() != Err(mpsc::TryRecvError::Empty) {
                return Found::Ended;
            }
            wait = LOOK;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.closer.close();
    }
}

/// Takes the lock, or fails where another process serves the database. A
/// request in use when the session is lost is let end for `drain`.
pub(super) fn alone(factory: StoreFactory, drain: Duration) -> std::io::Result<Alone> {
    let serving = begun(&factory, WAIT).map_err(|error| match error {
        Error::Precondition(SERVED) => std::io::Error::other(SERVED),
        _ => std::io::Error::other("the database is not reached"),
    })?;
    let session = Session::new(serving)
        .map_err(|_| std::io::Error::other("the session of the database is not held"))?;
    let (ended, serves) = mpsc::channel::<()>();
    let (lose, lost) = tokio::sync::oneshot::channel();
    let log = alt_server_telemetry::BackgroundLog::current();
    let watch = std::thread::spawn(move || {
        let (mut last, mut session) = (session.process, Some(session));
        // a timeout is the time to look; anything else is the end of serving
        while serves.recv_timeout(EVERY) == Err(RecvTimeoutError::Timeout) {
            let found = match &session {
                Some(session) => session.found(&factory, &serves),
                None => Found::Gone,
            };
            match found {
                Found::Held => continue,
                Found::Ended => return,
                Found::Gone => {}
            }
            if session.take().is_some() {
                // nothing new is served, and what is served ends, while the
                // session is still held where the database still has it
                factory.gate.close(std::time::Instant::now() + drain);
            }
            match begun(&factory, AGAIN).and_then(Session::new) {
                Ok(again) => {
                    last = again.process;
                    session = Some(again);
                    factory.gate.open();
                }
                Err(Error::Precondition(SERVED)) => match holder(&factory) {
                    // the session that was lost, which the database has not ended yet
                    Ok(Some(process)) if process == last => {}
                    _ => {
                        log.failed("serving", "", &SERVED);
                        let _ = lose.send(());
                        return;
                    }
                },
                // the database is away: nothing is written while it is
                Err(error) => log.failed("serving", "", &error),
            }
        }
    });
    Ok(Alone {
        _serving: ended,
        watch,
        lost,
    })
}

impl Alone {
    /// Ends the session, and with it the lock, before it returns.
    pub(super) async fn ended(self) -> std::io::Result<()> {
        let Alone {
            _serving, watch, ..
        } = self;
        drop(_serving);
        tokio::task::spawn_blocking(move || watch.join())
            .await
            .map_err(|_| std::io::Error::other("serving session cleanup failed"))?
            .map_err(|_| std::io::Error::other("serving session cleanup failed"))
    }
}
