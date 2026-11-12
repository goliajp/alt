use crate::{
    Client, Result, Row, ToSql,
    client::{Fetch, at_most_one},
};

/// a transaction block on a borrowed connection, rolled back when dropped
/// without [`Transaction::commit`]
pub struct Transaction<'a> {
    client: &'a mut Client,
    cached: bool,
    finished: bool,
}

impl<'a> Transaction<'a> {
    pub(crate) fn new(client: &'a mut Client) -> Self {
        Self {
            client,
            cached: true,
            finished: false,
        }
    }

    /// prepares each statement for one use only, for work that changes the
    /// tables its own statements read
    pub fn uncached(mut self) -> Self {
        self.cached = false;
        self
    }

    pub fn query(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        self.client
            .run(sql, params, self.cached, Fetch::Rows(&mut rows))?;
        Ok(rows)
    }

    pub fn query_opt(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>> {
        at_most_one(self.query(sql, params)?)
    }

    pub fn query_one(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row> {
        self.query_opt(sql, params)?.ok_or(crate::Error::RowCount)
    }

    pub fn execute(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64> {
        self.client.run(sql, params, self.cached, Fetch::Count)
    }

    pub fn batch_execute(&mut self, sql: &str) -> Result<()> {
        self.client.batch_execute(sql)
    }

    /// see [`Client::clear_statements`]
    pub fn clear_statements(&mut self) {
        self.client.clear_statements();
    }

    pub fn commit(mut self) -> Result<()> {
        self.finish("COMMIT")
    }

    pub fn rollback(mut self) -> Result<()> {
        self.finish("ROLLBACK")
    }

    fn finish(&mut self, command: &str) -> Result<()> {
        self.finished = true;
        let link = &mut self.client.link;
        if link.begin {
            // nothing ran, so the block was never opened
            link.begin = false;
            return Ok(());
        }
        link.simple(command)
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        let link = &mut self.client.link;
        if self.finished {
            return;
        }
        if link.begin {
            link.begin = false;
        } else if !link.broken && link.status != b'I' {
            let _ = link.simple("ROLLBACK");
        }
    }
}
