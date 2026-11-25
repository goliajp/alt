//! Numbers as the database keeps them, and the time it tells.

use crate::database::Transaction;
use crate::{Error, Result};

pub(crate) fn integer(value: u64) -> Result<i64> {
    value
        .try_into()
        .map_err(|_| Error::Limit("database integer range"))
}
pub(crate) fn unsigned(value: i64) -> Result<u64> {
    value
        .try_into()
        .map_err(|_| Error::Integrity("negative database counter"))
}
pub(crate) fn now(tx: &mut Transaction<'_>) -> Result<u64> {
    unsigned(
        tx.query_one(
            "SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint",
            &[],
        )?
        .get(0),
    )
}
