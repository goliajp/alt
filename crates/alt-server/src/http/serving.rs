//! The session by which this process is the one that serves its database,
//! looked at every so often for as long as it serves.
//!
//! The lock lives as long as the session. A session the database ended is
//! begun again, and the lock asked for again: until it is had, another process
//! may have taken it, and then this one serves no longer.

use super::StoreFactory;
use alt_server_store::{Error, SERVED, Serving};
use std::{sync::mpsc, time::Duration};

/// how long a process that begins waits for the one before it to be gone
const WAIT: Duration = Duration::from_secs(2);
/// How long a process whose session was lost waits for the lock: as long as
/// the database takes to end a session it hears nothing of, which may be the
/// one that was lost, and a little more.
const AGAIN: Duration = alt_server_store::UNHEARD.saturating_add(Duration::from_secs(5));
/// how often the session is looked at
const EVERY: Duration = Duration::from_secs(5);

pub(super) struct Alone {
    /// dropped when the process serves no longer: the watch ends
    _serving: mpsc::Sender<()>,
    watch: std::thread::JoinHandle<()>,
    /// told when another process has the lock
    pub(super) lost: tokio::sync::oneshot::Receiver<()>,
}

fn begun(factory: &StoreFactory, wait: Duration) -> alt_server_store::Result<Serving> {
    factory.open()?.serve_alone(wait)
}

/// Takes the lock, or fails where another process serves the database.
pub(super) fn alone(factory: StoreFactory) -> std::io::Result<Alone> {
    let serving = begun(&factory, WAIT).map_err(|error| match error {
        Error::Precondition(SERVED) => std::io::Error::other(SERVED),
        _ => std::io::Error::other("the database is not reached"),
    })?;
    let (ended, serves) = mpsc::channel::<()>();
    let (lose, lost) = tokio::sync::oneshot::channel();
    let log = alt_server_telemetry::BackgroundLog::current();
    let watch = std::thread::spawn(move || {
        let mut serving = Some(serving);
        // a timeout is the time to look; anything else is the end of serving
        while serves.recv_timeout(EVERY) == Err(mpsc::RecvTimeoutError::Timeout) {
            let held = serving.as_mut().map(Serving::held);
            if held.is_some_and(|held| held.is_ok()) {
                continue;
            }
            // the session that was lost ends here, where the database still has it
            serving = None;
            match begun(&factory, AGAIN) {
                Ok(again) => serving = Some(again),
                Err(Error::Precondition(SERVED)) => {
                    log.failed("serving", "", &SERVED);
                    let _ = lose.send(());
                    return;
                }
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
