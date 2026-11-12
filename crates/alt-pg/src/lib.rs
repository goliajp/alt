//! A blocking PostgreSQL client for wire protocol 3.
//!
//! Statements run through the extended protocol with binary parameters and
//! results; each SQL text is prepared once per connection. A request and its
//! response take one round trip, and a transaction block opens in the same
//! round trip as its first statement.
//!
//! Connections are plain TCP or Unix sockets. Authentication is `trust` or
//! SCRAM-SHA-256.

mod client;
mod config;
mod connect;
mod error;
mod generic;
mod link;
mod row;
mod scram;
mod statement;
mod stream;
mod transaction;
mod types;
mod wire;

pub use client::Client;
pub use config::{Config, Host};
pub use error::{Error, Result, ServerError};
pub use generic::GenericClient;
pub use row::{Column, Row, RowIndex};
pub use statement::STATEMENT_LIMIT;
pub use transaction::Transaction;
pub use types::{FromSql, IsNull, Oid, ToSql, WRONG_TYPE, oid};
