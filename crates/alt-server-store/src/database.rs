//! Connection-local statement reuse. Only SQL plans are retained; result rows,
//! authority decisions, transaction boundaries and errors are never cached.
use alt_pg::{Client, Error, Row, ToSql};

pub(crate) const BATCH_ROWS: usize = 1024;

/// how far the recorded clock of a Cloud instance may be behind before a read moves it on
const CLOCK_STEP: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) struct Connection {
    client: Client,
    /// when this connection last moved the recorded clock of a Cloud instance on
    clock_moved: Option<std::time::Instant>,
}
impl Connection {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            clock_moved: None,
        }
    }
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }
    /// one statement outside any transaction block, with the cached plan
    pub fn query_one(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row, Error> {
        self.client.query_one(sql, params)
    }
    /// one statement outside any transaction block; how many rows it wrote
    pub fn execute(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64, Error> {
        self.client.execute(sql, params)
    }
    /// one statement outside any transaction block, planned for these parameter values
    pub fn query_one_for_values(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Row, Error> {
        let mut rows = self.client.query_for_values(sql, params)?.into_iter();
        match (rows.next(), rows.next()) {
            (Some(row), None) => Ok(row),
            _ => Err(Error::RowCount),
        }
    }
    pub fn transaction(&mut self) -> Result<Transaction<'_>, Error> {
        Ok(Transaction {
            inner: self.client.transaction()?,
            cached: true,
            reading: false,
        })
    }
    /// A transaction that only reads, under the one view it takes as it begins.
    ///
    /// A Cloud instance records the highest clock it has seen, so that a clock
    /// set back gives no lease more life. A read writes no row, or it would wait
    /// for every other and fail where another came first; the clock is moved on
    /// here instead, by a statement of its own before the read begins and no
    /// more often than once in [`CLOCK_STEP`] on a connection. Where another
    /// transaction has the row, that one records the clock and this statement
    /// leaves it alone. The read then holds the clock to what is recorded like
    /// every transaction. So what is recorded may be behind by that step and by
    /// as long as the transaction that has the row takes to commit: a clock set
    /// back by less than that is not noticed by a read.
    pub fn reading(&mut self, cloud: bool) -> Result<Transaction<'_>, Error> {
        if cloud && self.clock_moved.is_none_or(|at| at.elapsed() >= CLOCK_STEP) {
            self.client.execute("WITH free AS (SELECT singleton FROM alt_cloud_binding WHERE singleton FOR UPDATE SKIP LOCKED) UPDATE alt_cloud_binding b SET clock_high_water=t.now FROM free,(SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint AS now) t WHERE b.singleton AND b.clock_high_water<t.now", &[])?;
            self.clock_moved = Some(std::time::Instant::now());
        }
        let mut tx = self.transaction()?;
        // as a statement like the others: the plans the connection keeps
        // for them are kept
        tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", &[])?;
        tx.reading = true;
        Ok(tx)
    }
}

pub(crate) struct Transaction<'a> {
    inner: alt_pg::Transaction<'a>,
    cached: bool,
    /// under one view and writing nothing; see [`Connection::reading`]
    reading: bool,
}
impl<'a> Transaction<'a> {
    /// Operator initialization/migration/backup may borrow an external client.
    /// They keep its ordinary transaction lifetime without retaining plans.
    pub fn uncached(inner: alt_pg::Transaction<'a>) -> Self {
        Self {
            inner: inner.uncached(),
            cached: false,
            reading: false,
        }
    }
    pub fn is_reading(&self) -> bool {
        self.reading
    }
    pub fn raw_mut(&mut self) -> &mut alt_pg::Transaction<'a> {
        // Raw schema operations may change statement result types or settings.
        if self.cached {
            self.inner.clear_statements();
        }
        &mut self.inner
    }
    pub fn query(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error> {
        self.inner.query(sql, params)
    }
    pub fn query_opt(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, Error> {
        self.inner.query_opt(sql, params)
    }
    pub fn query_one(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row, Error> {
        self.inner.query_one(sql, params)
    }
    pub fn execute(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64, Error> {
        self.inner.execute(sql, params)
    }
    /// planned for these parameter values; the other statements of the transaction are not
    pub fn query_for_values(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error> {
        self.inner.query_for_values(sql, params)
    }
    pub fn execute_for_values(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, Error> {
        self.inner.execute_for_values(sql, params)
    }
    pub fn batch_execute(&mut self, sql: &str) -> Result<(), Error> {
        self.raw_mut().batch_execute(sql)
    }
    pub fn commit(self) -> Result<(), Error> {
        self.inner.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_pg::STATEMENT_LIMIT;

    #[test]
    #[ignore = "requires an explicitly isolated PostgreSQL service"]
    fn cached_statements_keep_current_rows_and_survive_rollback() {
        assert_eq!(
            std::env::var("ALT_SERVER_TEST_ISOLATED").as_deref(),
            Ok("1")
        );
        let dsn = std::env::var("ALT_SERVER_TEST_ADMIN_DSN").unwrap();
        let connect = || Connection::new(Client::connect(&dsn).unwrap());
        let mut connection = connect();
        {
            let mut tx = connection.transaction().unwrap();
            tx.batch_execute("CREATE TEMP TABLE cache_test(value bigint NOT NULL)")
                .unwrap();
            tx.execute("INSERT INTO cache_test VALUES($1)", &[&1i64])
                .unwrap();
            tx.commit().unwrap();
        }
        let sql = "SELECT value FROM cache_test";
        let mut names = Vec::new();
        for expected in [1i64, 2] {
            let mut tx = connection.transaction().unwrap();
            if expected == 2 {
                tx.execute("UPDATE cache_test SET value=$1", &[&expected])
                    .unwrap();
            }
            assert_eq!(tx.query_one(sql, &[]).unwrap().get::<_, i64>(0), expected);
            names.push(
                tx.query_one(
                    "SELECT name FROM pg_prepared_statements WHERE statement=$1",
                    &[&sql],
                )
                .unwrap()
                .get::<_, String>(0),
            );
            tx.commit().unwrap();
        }
        assert_eq!(names[0], names[1]);
        {
            let mut tx = connection.transaction().unwrap();
            tx.execute("UPDATE cache_test SET value=$1", &[&99i64])
                .unwrap();
            assert!(tx.query_one("SELECT 1 / $1::integer", &[&0i32]).is_err());
            // Drop rolls back. No failed write is retried by the cache.
        }
        let mut tx = connection.transaction().unwrap();
        assert_eq!(tx.query_one(sql, &[]).unwrap().get::<_, i64>(0), 2);
        assert_eq!(
            tx.query_one("SELECT 1 / $1::integer", &[&1i32])
                .unwrap()
                .get::<_, i32>(0),
            1
        );
        tx.batch_execute("ALTER TABLE cache_test ALTER COLUMN value TYPE text")
            .unwrap();
        assert_eq!(tx.query_one(sql, &[]).unwrap().get::<_, String>(0), "2");
        tx.commit().unwrap();
        for value in 0..STATEMENT_LIMIT + 20 {
            let mut tx = connection.transaction().unwrap();
            assert_eq!(
                tx.query_one(&format!("SELECT {value}::bigint"), &[])
                    .unwrap()
                    .get::<_, i64>(0),
                value as i64
            );
            tx.commit().unwrap();
        }
        assert_eq!(connection.client.cached_statements(), STATEMENT_LIMIT);
        let mut other = connect();
        assert_eq!(other.client.cached_statements(), 0);
        assert_eq!(
            other
                .transaction()
                .unwrap()
                .query_one("SELECT 1 / $1::integer", &[&1i32])
                .unwrap()
                .get::<_, i32>(0),
            1
        );
    }
}
