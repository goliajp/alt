use crate::{
    Config, Error, Result, Row, ToSql,
    link::{Link, Sink},
    row::RowSet,
    statement::Statements,
    wire,
};

/// a blocking connection to one PostgreSQL server
///
/// Statements are prepared once per SQL text and kept for the life of the
/// connection, oldest dropped first past [`crate::STATEMENT_LIMIT`].
pub struct Client {
    pub(crate) link: Link,
    statements: Statements,
}

pub(crate) enum Fetch<'a> {
    Count,
    Rows(&'a mut Vec<Row>),
}

impl Client {
    /// connects using `key=value` connection parameters
    pub fn connect(parameters: &str) -> Result<Self> {
        Self::connect_with(&parameters.parse()?)
    }

    pub fn connect_with(config: &Config) -> Result<Self> {
        Ok(Self {
            link: Link::open(config)?,
            statements: Statements::default(),
        })
    }

    /// true once an I/O failure or a malformed message ended the connection
    pub fn is_closed(&self) -> bool {
        self.link.broken
    }

    pub fn query(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        self.run(sql, params, true, Fetch::Rows(&mut rows))?;
        Ok(rows)
    }

    pub fn query_opt(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>> {
        at_most_one(self.query(sql, params)?)
    }

    pub fn query_one(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row> {
        self.query_opt(sql, params)?.ok_or(Error::RowCount)
    }

    /// runs a statement and returns the number of rows it affected
    pub fn execute(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64> {
        self.run(sql, params, true, Fetch::Count)
    }

    /// runs one or more statements without parameters, discarding any rows
    pub fn batch_execute(&mut self, sql: &str) -> Result<()> {
        self.link.simple(sql)
    }

    /// a transaction block; it is opened on the server together with its
    /// first statement and rolled back unless committed
    pub fn transaction(&mut self) -> Result<crate::Transaction<'_>> {
        if self.link.broken {
            return Err(Error::Closed);
        }
        self.link.begin = true;
        Ok(crate::Transaction::new(self))
    }

    /// drops every prepared statement, as needed after changing the type of
    /// a column a kept statement returns
    pub fn clear_statements(&mut self) {
        self.link.closing.extend(self.statements.clear());
    }

    pub fn cached_statements(&self) -> usize {
        self.statements.len()
    }

    pub(crate) fn run(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
        cached: bool,
        fetch: Fetch<'_>,
    ) -> Result<u64> {
        let unnamed;
        let statement = if !cached {
            unnamed = self.link.prepare(sql, 0)?;
            &unnamed
        } else {
            if self.statements.get(sql).is_none() {
                let id = self.statements.next_id();
                let statement = self.link.prepare(sql, id)?;
                self.link
                    .closing
                    .extend(self.statements.insert(sql, statement));
            }
            self.statements.get(sql).expect("statement just kept")
        };
        match fetch {
            Fetch::Count => self.link.execute(statement, params, Sink::Discard),
            Fetch::Rows(rows) => {
                let mut set = RowSet::new(statement.columns.clone());
                let affected = self.link.execute(statement, params, Sink::Rows(&mut set))?;
                *rows = set.into_rows();
                Ok(affected)
            }
        }
    }
}

pub(crate) fn at_most_one(rows: Vec<Row>) -> Result<Option<Row>> {
    let mut rows = rows.into_iter();
    match (rows.next(), rows.next()) {
        (row, None) => Ok(row),
        _ => Err(Error::RowCount),
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if !self.link.broken {
            self.link.out.clear();
            wire::terminate(&mut self.link.out);
            let _ = self.link.stream.send(&self.link.out);
        }
    }
}
