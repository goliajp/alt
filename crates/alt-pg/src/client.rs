use crate::{
    Config, Error, Result, Row, ToSql,
    link::{Link, Sink},
    row::RowSet,
    statement::{Statement, Statements},
    wire,
};

/// a blocking connection to one PostgreSQL server
///
/// Statements are prepared once per SQL text and kept for the life of the
/// connection, oldest dropped first past [`crate::STATEMENT_LIMIT`].
pub struct Client {
    pub(crate) link: Link,
    statements: Statements,
    /// what switches planning to the parameter values at hand and back, once prepared
    planning: Option<(Statement, Statement)>,
}

/// The setting is local to the transaction; the earlier value is kept aside so that
/// the rest of an open transaction goes on with it.
const PLAN_FOR_VALUES: &str = "SELECT set_config('alt_pg.plan_cache_mode',current_setting('plan_cache_mode'),true),set_config('plan_cache_mode','force_custom_plan',true)";
const PLAN_AS_BEFORE: &str =
    "SELECT set_config('plan_cache_mode',current_setting('alt_pg.plan_cache_mode'),true)";

/// how a statement is planned
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plan {
    /// as the session's `plan_cache_mode` has it
    Session,
    /// for the parameter values of this execution
    ForValues,
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
            planning: None,
        })
    }

    /// true once an I/O failure or a malformed message ended the connection
    pub fn is_closed(&self) -> bool {
        self.link.broken
    }

    pub fn query(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        self.run(sql, params, true, Plan::Session, Fetch::Rows(&mut rows))?;
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
        self.run(sql, params, true, Plan::Session, Fetch::Count)
    }

    /// Like [`Client::query`], planned for these parameter values whatever the
    /// session's `plan_cache_mode` says, in the same single round trip. For a
    /// statement whose cost depends on the size of an array it is given.
    pub fn query_for_values(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        self.run(sql, params, true, Plan::ForValues, Fetch::Rows(&mut rows))?;
        Ok(rows)
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
        plan: Plan,
        fetch: Fetch<'_>,
    ) -> Result<u64> {
        if plan == Plan::ForValues && self.planning.is_none() {
            let before = self.statements.next_id();
            let before = self.link.prepare(PLAN_FOR_VALUES, before)?;
            let after = self.statements.next_id();
            let after = self.link.prepare(PLAN_AS_BEFORE, after)?;
            self.planning = Some((before, after));
        }
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
        let around = match (plan, &self.planning) {
            (Plan::ForValues, Some((before, after))) => Some((before, after)),
            _ => None,
        };
        match fetch {
            Fetch::Count => self.link.execute(statement, params, around, Sink::Discard),
            Fetch::Rows(rows) => {
                let mut set = RowSet::new(statement.columns.clone());
                let affected =
                    self.link
                        .execute(statement, params, around, Sink::Rows(&mut set))?;
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
