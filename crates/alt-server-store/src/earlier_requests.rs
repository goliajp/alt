//! Requests that ended before their moment was recorded are given it as the
//! server runs.
//!
//! The upgrade to schema 22 adds the column and writes no row: a table whose
//! every row is written in one transaction is there twice afterwards. The rows
//! are written here instead, so many in a transaction, and the table is vacuumed
//! after every so many of them, so that the versions left behind are written
//! over by the rows that come next. A committed request is given the moment in
//! its receipt; one that was rejected or expired is given the moment it is come
//! to, which is no sooner than it ended. Until a request has its moment it is
//! not removed: the removal goes by the moment.

use crate::store::{integer, unsigned};
use crate::{Error, Result, Store};

const GIVE: &str = "UPDATE alt_requests q SET settled_at=CASE WHEN q.state='committed' THEN (q.result#>>'{receipt,body,committed_at_unix_ms}')::bigint ELSE $1 END FROM (SELECT repository,epoch,actor_id,request_id FROM alt_requests WHERE settled_at IS NULL AND state IN ('committed','rejected','expired') ORDER BY expires_at LIMIT $2 FOR UPDATE SKIP LOCKED) s WHERE q.repository=s.repository AND q.epoch=s.epoch AND q.actor_id=s.actor_id AND q.request_id=s.request_id";

impl Store {
    /// How many requests that are over have not been given their moment yet.
    pub fn requests_without_moment(&mut self) -> Result<u64> {
        let waiting = "SELECT count(*) FROM alt_requests WHERE settled_at IS NULL AND state IN ('committed','rejected','expired')";
        unsigned(self.client.query_one(waiting, &[])?.get(0))
    }

    /// Gives their moment to up to `rows` requests in each of up to `transactions`
    /// transactions, then vacuums the table if any was written. How many were
    /// given theirs, and whether there may be more for the next round: the last
    /// transaction wrote as many as it may, or a request is left that another
    /// transaction had locked and this one passed.
    pub fn give_earlier_requests_their_moment(
        &mut self,
        rows: u32,
        transactions: u32,
    ) -> Result<(u64, bool)> {
        let (mut given, mut more) = (0, false);
        for _ in 0..transactions {
            let count = self.give_moments(i64::from(rows))?;
            given += count;
            more = count >= u64::from(rows);
            if !more {
                break;
            }
        }
        if given > 0 {
            self.vacuum_requests()?;
        }
        if !more {
            let left = "SELECT EXISTS(SELECT 1 FROM alt_requests WHERE settled_at IS NULL AND state IN ('committed','rejected','expired'))";
            more = self.client.query_one(left, &[])?.get(0);
        }
        Ok((given, more))
    }

    /// Outside any transaction, as a vacuum has to be, and without the time a
    /// statement of this connection is given: it reads the whole table the first
    /// time, and keeps nobody from the table while it does.
    fn vacuum_requests(&mut self) -> Result<()> {
        let held: String = (self.client)
            .query_one("SELECT current_setting('statement_timeout')", &[])?
            .get(0);
        let set = "SELECT set_config('statement_timeout',$1,false)";
        self.client.query_one(set, &[&"0"])?;
        let vacuumed = self.client.execute("VACUUM alt_requests", &[]);
        self.client.query_one(set, &[&held])?;
        vacuumed?;
        Ok(())
    }

    fn give_moments(&mut self, rows: i64) -> Result<u64> {
        let (mut tx, time) = self.removing()?;
        let count = tx.execute(GIVE, &[&integer(time)?, &rows])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok(count)
    }
}
