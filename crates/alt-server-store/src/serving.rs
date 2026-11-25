//! One process serves the database of an instance at a time.
//!
//! What a serving process keeps of its own (the files it is writing, the
//! segments it has open, what it is merging) no other process sees, on the
//! same host or on another one that reaches the same database and the same
//! bytes. The process that serves therefore holds a lock of the database for
//! as long as a session of its own lives, and a second one finds it held
//! wherever it runs.

use crate::{Error, Result, Store, database::Connection};
use std::time::Duration;

pub const SERVED: &str = "another server serves this database";

/// the lock of the database a serving process holds
const SERVES: i64 = 715_071_909_112;
/// How long the database hears nothing of the session that holds the lock
/// before it ends it, and lets go of the lock: ten seconds of silence, then
/// three probes five seconds apart. On a socket of the host the database sees
/// the session end as the process does, and this does not come into it.
pub const UNHEARD: Duration = Duration::from_secs(25);
const KEEPALIVES: &str =
    "SET tcp_keepalives_idle=10; SET tcp_keepalives_interval=5; SET tcp_keepalives_count=3";
/// how long the process waits for the database to answer whether the session is there
const ANSWER: Duration = Duration::from_secs(10);

/// The session that holds the lock, which ends with it.
pub struct Serving(Connection);

impl Store {
    /// Makes the connection of this store the session of the process that
    /// serves. A process that served until now may still be ending its own:
    /// that is waited for, for `wait`.
    ///
    /// A session the process hears nothing of in [`ANSWER`] is one it holds
    /// no longer, whether the database still has it or not: it ends it, and
    /// begins another.
    pub fn serve_alone(mut self, wait: Duration) -> Result<Serving> {
        // every answer is waited for no longer than the lock and the time to
        // answer: once the lock is had nothing more is asked
        self.client.set_read_timeout(wait.saturating_add(ANSWER))?;
        let (wait, most) = (wait.as_millis(), wait.as_millis() + 1000);
        let set = format!("SET lock_timeout={wait}; SET statement_timeout={most}; {KEEPALIVES}");
        self.client.batch_execute(&set)?;
        match (self.client).query_one("SELECT pg_advisory_lock($1)", &[&SERVES]) {
            Ok(_) => {
                self.client.set_read_timeout(ANSWER)?;
                Ok(Serving(self.client))
            }
            // the wait for the lock is over, or the statement's own time
            Err(error) if matches!(error.code(), Some("55P03" | "57014")) => {
                Err(Error::Precondition(SERVED))
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Serving {
    /// Fails once the session is gone, and the lock with it.
    pub fn held(&mut self) -> Result<()> {
        self.0.query_one("SELECT 1", &[])?;
        Ok(())
    }
}
