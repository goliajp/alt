//! Tests against a real server: run through `scripts/with-test-postgres.sh`.

use alt_pg::{Client, FromSql, IsNull, Oid, ToSql, WRONG_TYPE, oid};

mod auth;
mod cache;
mod failures;
mod results;
mod transactions;
mod types;

fn admin() -> String {
    assert_eq!(
        std::env::var("ALT_SERVER_TEST_ISOLATED").as_deref(),
        Ok("1"),
        "database tests require an explicitly isolated PostgreSQL service"
    );
    std::env::var("ALT_SERVER_TEST_ADMIN_DSN").expect("isolated PostgreSQL admin DSN required")
}

fn connect() -> Client {
    Client::connect(&admin()).unwrap()
}

fn code<T: std::fmt::Debug>(result: alt_pg::Result<T>) -> String {
    result.unwrap_err().code().expect("a server error").into()
}

/// a JSON document as text, the way a caller with its own JSON type plugs in
#[derive(Debug, PartialEq)]
struct Json(String);

impl ToSql for Json {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        match ty {
            oid::JSONB => out.push(1),
            oid::JSON => {}
            _ => return Err(WRONG_TYPE),
        }
        out.extend_from_slice(self.0.as_bytes());
        Ok(IsNull::No)
    }
}

impl FromSql<'_> for Json {
    fn accepts(ty: Oid) -> bool {
        matches!(ty, oid::JSON | oid::JSONB)
    }
    fn from_sql(ty: Oid, raw: &[u8]) -> Result<Self, String> {
        let text = if ty == oid::JSONB { &raw[1..] } else { raw };
        Ok(Self(
            String::from_utf8(text.to_vec()).map_err(|error| error.to_string())?,
        ))
    }
}
