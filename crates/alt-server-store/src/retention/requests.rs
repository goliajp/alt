//! The requests that are over long enough, removed so many at a time.

use super::*;

/// the request behind `seen` that is over long enough, whether it goes or stays
const NEXT: &str = "SELECT q.settled_at,q.repository,q.epoch,q.actor_id,q.request_id FROM alt_requests q WHERE q.settled_at IS NOT NULL AND q.settled_at<=$1 AND q.settled_at>=$2 AND (q.settled_at,q.repository,q.epoch,q.actor_id,q.request_id)>($2,$3,$4,$5,$6) ORDER BY q.settled_at,q.repository,q.epoch,q.actor_id,q.request_id LIMIT 1";

/// Looks at up to `limit` requests behind `seen` and removes those that do not
/// stay: how many went, and whether there are more to look at.
fn looked_at(store: &mut Store, limit: i64, seen: &mut RequestsSeen) -> Result<(u64, bool)> {
    let (mut tx, time) = store.removing()?;
    let before = integer(time.saturating_sub(REQUEST_RETENTION_MS))?;
    let row = tx.query_opt(
        REQUESTS,
        &[
            &before,
            &seen.settled_at,
            &seen.repository,
            &seen.epoch,
            &seen.actor,
            &seen.request,
            &limit,
        ],
    )?;
    tx.commit().map_err(|_| Error::OutcomeUnknown)?;
    let Some(row) = row else {
        *seen = RequestsSeen::default();
        return Ok((0, false));
    };
    let more = row.get::<_, i64>(6) >= limit;
    *seen = match more {
        true => RequestsSeen {
            settled_at: row.get(0),
            repository: row.get(1),
            epoch: row.get(2),
            actor: row.get(3),
            request: row.get(4),
        },
        false => RequestsSeen::default(),
    };
    Ok((unsigned(row.get(5))?, more))
}

/// Has `seen` stand at the request behind it, without that request being looked at.
fn passed(store: &mut Store, seen: &mut RequestsSeen) -> Result<()> {
    let (mut tx, time) = store.removing()?;
    let before = integer(time.saturating_sub(REQUEST_RETENTION_MS))?;
    let row = tx.query_opt(
        NEXT,
        &[
            &before,
            &seen.settled_at,
            &seen.repository,
            &seen.epoch,
            &seen.actor,
            &seen.request,
        ],
    )?;
    tx.commit()?;
    *seen = row.map_or_else(RequestsSeen::default, |row| RequestsSeen {
        settled_at: row.get(0),
        repository: row.get(1),
        epoch: row.get(2),
        actor: row.get(3),
        request: row.get(4),
    });
    Ok(())
}

/// One round of the rule. The requests of a round go in one statement, and one
/// that the database refuses to remove fails it for all of them: the round is
/// then gone through a request at a time, the one that does not go is told to
/// `failed` and passed, and those behind it go as they would have. It is looked
/// at again in the next pass, as the requests that stay are.
pub(super) fn over(
    store: &mut Store,
    limit: i64,
    seen: &mut RequestsSeen,
    failed: &mut dyn FnMut(&str, Error),
) -> (u64, bool) {
    match looked_at(store, limit, seen) {
        Ok(round) => return round,
        Err(Error::Database(_)) => {}
        Err(error) => {
            failed("requests", error);
            return (0, false);
        }
    }
    let mut removed = 0;
    for _ in 0..limit {
        match looked_at(store, 1, seen) {
            Ok((count, true)) => removed += count,
            Ok((count, false)) => return (removed + count, false),
            Err(error) => {
                let stands = passed(store, seen);
                failed("requests", error);
                if let Err(error) = stands {
                    failed("requests", error);
                    return (removed, false);
                }
                // it was the last: the next round begins the next pass
                if seen.repository.is_empty() {
                    return (removed, false);
                }
            }
        }
    }
    (removed, true)
}
