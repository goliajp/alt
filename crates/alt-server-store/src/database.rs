//! Connection-local statement reuse. Only SQL plans are retained; result rows,
//! authority decisions, transaction boundaries and errors are never cached.
use postgres::{Client, Error, Row, Statement, types::ToSql};
use std::collections::{HashMap, VecDeque};

const STATEMENT_LIMIT: usize = 256;
pub(crate) const BATCH_ROWS: usize = 1024;

#[derive(Default)]
struct Statements {
    values: HashMap<String, Statement>,
    order: VecDeque<String>,
}
impl Statements {
    fn clear(&mut self) {
        self.values.clear();
        self.order.clear();
    }
    fn insert(&mut self, sql: &str, statement: Statement) {
        if self.values.len() == STATEMENT_LIMIT {
            let oldest = self.order.pop_front().expect("statement cache order");
            self.values.remove(&oldest);
        }
        self.order.push_back(sql.to_owned());
        self.values.insert(sql.to_owned(), statement);
    }
}

pub(crate) struct Connection {
    client: Client,
    statements: Statements,
}
impl Connection {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            statements: Statements::default(),
        }
    }
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }
    /// one statement outside any transaction block, with the cached plan
    pub fn query_one(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row, Error> {
        let statement = match self.statements.values.get(sql) {
            Some(statement) => statement.clone(),
            None => {
                let statement = self.client.prepare(sql)?;
                self.statements.insert(sql, statement.clone());
                statement
            }
        };
        self.client.query_one(&statement, params)
    }
    pub fn transaction(&mut self) -> Result<Transaction<'_>, Error> {
        Ok(Transaction {
            inner: self.client.transaction()?,
            statements: Some(&mut self.statements),
        })
    }
}

pub(crate) struct Transaction<'a> {
    inner: postgres::Transaction<'a>,
    statements: Option<&'a mut Statements>,
}
impl<'a> Transaction<'a> {
    /// Operator initialization/migration/backup may borrow an external client.
    /// They keep its ordinary transaction lifetime without a cross-client cache.
    pub fn uncached(inner: postgres::Transaction<'a>) -> Self {
        Self {
            inner,
            statements: None,
        }
    }
    pub fn raw_mut(&mut self) -> &mut postgres::Transaction<'a> {
        // Raw schema operations may change statement result types or settings.
        if let Some(statements) = &mut self.statements {
            statements.clear();
        }
        &mut self.inner
    }
    fn statement(&mut self, sql: &str) -> Result<Statement, Error> {
        if let Some(statements) = &self.statements
            && let Some(statement) = statements.values.get(sql)
        {
            return Ok(statement.clone());
        }
        let statement = self.inner.prepare(sql)?;
        if let Some(statements) = &mut self.statements {
            statements.insert(sql, statement.clone());
        }
        Ok(statement)
    }
    pub fn query(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error> {
        let statement = self.statement(sql)?;
        self.inner.query(&statement, params)
    }
    pub fn query_opt(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, Error> {
        let statement = self.statement(sql)?;
        self.inner.query_opt(&statement, params)
    }
    pub fn query_one(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row, Error> {
        let statement = self.statement(sql)?;
        self.inner.query_one(&statement, params)
    }
    pub fn execute(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64, Error> {
        let statement = self.statement(sql)?;
        self.inner.execute(&statement, params)
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

    #[test]
    #[ignore = "requires an explicitly isolated PostgreSQL service"]
    fn cached_statements_keep_current_rows_and_survive_rollback() {
        assert_eq!(
            std::env::var("ALT_SERVER_TEST_ISOLATED").as_deref(),
            Ok("1")
        );
        let dsn = std::env::var("ALT_SERVER_TEST_ADMIN_DSN").unwrap();
        let connect = || Connection::new(Client::connect(&dsn, postgres::NoTls).unwrap());
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
        assert_eq!(connection.statements.values.len(), STATEMENT_LIMIT);
        let mut other = connect();
        assert!(other.statements.values.is_empty());
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
