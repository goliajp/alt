//! The data step of schema 20. Older versions stored the publication of a committed
//! request three more times: inside the receipt of the request's stored state, of
//! its audit record and of its outbox event. Here the stored state and the outbox
//! event give theirs up. Each rewritten row is held to what it was: read back the
//! way this version reads it, it has to be the document it replaced.

use crate::{
    Error, Result,
    committed::{outbox_event_sql, state_sql},
    database::Transaction,
};

/// Runs inside the migration transaction. A row that is already in the new form is
/// left as it is.
pub(crate) fn shorten(tx: &mut Transaction<'_>) -> Result<()> {
    let requests = tx.query_one(
        concat!(
            "WITH rewritten AS (UPDATE alt_requests q SET result=q.result #- '{receipt,body,publication}' WHERE q.state='committed' AND q.result #> '{receipt,body,publication}' IS NOT NULL RETURNING q.repository,",
            state_sql!(),
            " IS DISTINCT FROM old.result AS differs) SELECT min(repository) FROM rewritten WHERE differs"
        ),
        &[],
    )?;
    held(requests, "committed request differs from its publication")?;
    let events = tx.query_one(
        concat!(
            "WITH rewritten AS (UPDATE alt_outbox o SET event=jsonb_build_object('kind',o.event->'kind','request',o.event#>'{receipt,body,publication,key}','receipt',(o.event->'receipt') #- '{body,publication}') WHERE o.event->>'kind'='publication_committed' AND o.event #> '{receipt,body,publication}' IS NOT NULL RETURNING o.repository,",
            outbox_event_sql!(),
            " IS DISTINCT FROM old.event AS differs) SELECT min(repository) FROM rewritten WHERE differs"
        ),
        &[],
    )?;
    held(events, "outbox event differs from its request")
}

/// fails the migration when a rewritten row of some repository does not read back
fn held(row: alt_pg::Row, what: &'static str) -> Result<()> {
    match row.get::<_, Option<String>>(0) {
        None => Ok(()),
        Some(repository) => {
            let error = Error::Integrity(what);
            alt_server_telemetry::BackgroundLog::current().failed(
                "schema 20 migration",
                &repository,
                &error,
            );
            Err(error)
        }
    }
}
