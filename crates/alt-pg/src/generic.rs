use crate::{Client, Result, Row, ToSql, Transaction};

/// what a connection and a transaction on it both do, for code that runs
/// either inside a caller's transaction or on its own
pub trait GenericClient {
    fn query(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>>;
    fn query_opt(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>>;
    fn query_one(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row>;
    fn execute(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64>;
    fn batch_execute(&mut self, sql: &str) -> Result<()>;
}

macro_rules! forward {
    ($($target:ty),*) => {$(
        impl GenericClient for $target {
            fn query(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
                <$target>::query(self, sql, params)
            }
            fn query_opt(
                &mut self,
                sql: &str,
                params: &[&(dyn ToSql + Sync)],
            ) -> Result<Option<Row>> {
                <$target>::query_opt(self, sql, params)
            }
            fn query_one(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row> {
                <$target>::query_one(self, sql, params)
            }
            fn execute(&mut self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64> {
                <$target>::execute(self, sql, params)
            }
            fn batch_execute(&mut self, sql: &str) -> Result<()> {
                <$target>::batch_execute(self, sql)
            }
        }
    )*};
}
forward!(Client, Transaction<'_>);
