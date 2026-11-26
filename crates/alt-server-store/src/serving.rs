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
/// how long the process waits for the database to answer while it begins a
/// session, and to tell who holds the lock
const ANSWER: Duration = Duration::from_secs(10);
/// the session of the database that holds the lock of the serving process
const HOLDER: &str = "SELECT (SELECT pid FROM pg_locks WHERE locktype='advisory' AND (classid::bigint<<32|objid::bigint)=$1 AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND granted LIMIT 1)";

/// The session that holds the lock, which ends with it, and its process in
/// the database.
pub struct Serving(Connection, i32);

impl Store {
    /// Makes the connection of this store the session of the process that
    /// serves. A process that served until now may still be ending its own:
    /// that is waited for, for `wait`.
    ///
    /// Once it holds the lock its answers are waited for without end: an
    /// answer that is slow is told from a session that is gone by asking
    /// another session who holds the lock ([`Store::serving_session`]).
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
                let session = self.client.query_one("SELECT pg_backend_pid()", &[])?;
                self.client.clear_read_timeout()?;
                Ok(Serving(self.client, session.get(0)))
            }
            // the wait for the lock is over, or the statement's own time
            Err(error) if matches!(error.code(), Some("55P03" | "57014")) => {
                Err(Error::Precondition(SERVED))
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Store {
    /// The process in the database of the session that holds the lock of the
    /// serving process, where one does: what tells a session whose answers
    /// are slow from one that is gone. Asked for [`ANSWER`] at the most.
    pub fn serving_session(mut self) -> Result<Option<i32>> {
        self.client.set_read_timeout(ANSWER)?;
        Ok(self.client.query_one(HOLDER, &[&SERVES])?.get(0))
    }
}

impl Serving {
    /// Fails once the session is gone, and the lock with it: the database
    /// ended it or the connection to it broke. An answer is waited for
    /// without end, or until [`Serving::closer`] ends the connection.
    pub fn held(&mut self) -> Result<()> {
        self.0.query_one("SELECT 1", &[])?;
        Ok(())
    }

    /// the process of the session in the database, as [`Store::serving_session`] tells it
    pub fn session(&self) -> i32 {
        self.1
    }

    /// what ends the session from another thread
    pub fn closer(&self) -> Result<alt_pg::Closer> {
        Ok(self.0.closer()?)
    }
}
