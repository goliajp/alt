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

/// The session that holds the lock, which ends with it.
pub struct Serving(Connection);

impl Store {
    /// Makes the connection of this store the session of the process that
    /// serves. A process that served until now may still be ending its own:
    /// that is waited for, for `wait`.
    pub fn serve_alone(mut self, wait: Duration) -> Result<Serving> {
        let wait = wait.as_millis();
        (self.client).execute(&format!("SET lock_timeout={wait}"), &[])?;
        match (self.client).query_one("SELECT pg_advisory_lock($1)", &[&SERVES]) {
            Ok(_) => Ok(Serving(self.client)),
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
