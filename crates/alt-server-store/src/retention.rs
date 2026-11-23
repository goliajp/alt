//! What is over is kept for a time and then removed, as the server runs.
//!
//! A request that was committed, rejected or expired is kept for
//! [`REQUEST_RETENTION_MS`] from that moment: for as long, its id is answered with
//! what became of it, and after that the same id is a new request. One whose
//! publication carried task records is their history and stays, and so does one
//! a stored segment still names. A request left unfinished by an epoch before a
//! recovery is told as it is while it is there, and goes as long after its time
//! to upload ran out.
//!
//! What stood for a sign-in (a browser session, a credential of a device, of the
//! platform or of a delegation, an approval that could be spent) goes
//! [`SESSION_RETENTION_MS`] after it ended. The credential under it goes
//! [`CREDENTIAL_RETENTION_MS`] after it ended and once nothing names it any more,
//! and never before it has expired: until then the row of a revoked credential
//! is all that tells its token from one never seen. The audit records are facts
//! and are never removed.
//!
//! Every rule removes a bounded number of rows in a transaction of its own.

use crate::database::Transaction;
use crate::store::{instance, integer, now, unsigned};
use crate::{Error, Result, Store};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// how long the id of a request that is over is answered as a repetition
pub const REQUEST_RETENTION_MS: u64 = 30 * DAY_MS;
/// how long what stood for a sign-in is kept after it ended
pub const SESSION_RETENTION_MS: u64 = 30 * DAY_MS;
/// how long a credential is kept after it ended
pub const CREDENTIAL_RETENTION_MS: u64 = 90 * DAY_MS;

/// The last request that was looked at. The ones that stay for good are passed
/// once, not found again by every round.
#[derive(Clone, Debug, Default)]
pub struct RequestsSeen {
    settled_at: i64,
    repository: String,
    epoch: i64,
    actor: String,
    request: String,
}

/// what one round removed, by the table it left
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub requests: u64,
    pub unfinished_requests: u64,
    pub approvals: u64,
    pub web_sessions: u64,
    pub client_credentials: u64,
    pub cloud_credentials: u64,
    pub delegations: u64,
    pub device_authorizations: u64,
    pub cloud_logins: u64,
    pub credentials: u64,
    /// some rule removed as many as it may in one round: there is more for the next
    pub more: bool,
}

macro_rules! names_request {
    ($table:literal, $alias:literal, $of:literal) => {
        concat!(
            "EXISTS(SELECT 1 FROM ",
            $table,
            " ",
            $alias,
            " WHERE ",
            $alias,
            ".repository=",
            $of,
            ".repository AND ",
            $alias,
            ".epoch=",
            $of,
            ".epoch AND ",
            $alias,
            ".actor_id=",
            $of,
            ".actor_id AND ",
            $alias,
            ".request_id=",
            $of,
            ".request_id)"
        )
    };
}
macro_rules! same_request {
    ($a:literal, $b:literal) => {
        concat!(
            $a,
            ".repository=",
            $b,
            ".repository AND ",
            $a,
            ".epoch=",
            $b,
            ".epoch AND ",
            $a,
            ".actor_id=",
            $b,
            ".actor_id AND ",
            $a,
            ".request_id=",
            $b,
            ".request_id"
        )
    };
}

const REQUESTS: &str = concat!(
    "WITH seen AS (SELECT q.repository,q.epoch,q.actor_id,q.request_id,q.settled_at,(",
    names_request!("alt_task_bundles", "b", "q"),
    " OR ",
    names_request!("alt_segments", "g", "q"),
    ") AS kept FROM alt_requests q WHERE q.settled_at IS NOT NULL AND q.settled_at<=$1 AND q.settled_at>=$2 AND (q.settled_at,q.repository,q.epoch,q.actor_id,q.request_id)>($2,$3,$4,$5,$6) ORDER BY q.settled_at,q.repository,q.epoch,q.actor_id,q.request_id LIMIT $7), approvals AS (DELETE FROM alt_current_approvals a USING seen s WHERE NOT s.kept AND ",
    same_request!("a", "s"),
    "), gone AS (DELETE FROM alt_requests q USING seen s WHERE NOT s.kept AND ",
    same_request!("q", "s"),
    " RETURNING 1) SELECT s.settled_at,s.repository,s.epoch,s.actor_id,s.request_id,(SELECT count(*) FROM gone),(SELECT count(*) FROM seen) FROM seen s ORDER BY s.settled_at DESC,s.repository DESC,s.epoch DESC,s.actor_id DESC,s.request_id DESC LIMIT 1"
);

const UNFINISHED: &str = concat!(
    "WITH seen AS (SELECT q.repository,q.epoch,q.actor_id,q.request_id FROM alt_requests q JOIN alt_repositories r ON r.id=q.repository WHERE q.state IN ('receiving','ready') AND q.epoch<r.recovery_epoch AND q.expires_at<=$1 AND NOT ",
    names_request!("alt_task_bundles", "b", "q"),
    " AND NOT ",
    names_request!("alt_segments", "g", "q"),
    " ORDER BY q.expires_at LIMIT $2), content AS (DELETE FROM alt_request_content c USING seen s WHERE ",
    same_request!("c", "s"),
    "), approvals AS (DELETE FROM alt_current_approvals a USING seen s WHERE ",
    same_request!("a", "s"),
    "), gone AS (DELETE FROM alt_requests q USING seen s WHERE ",
    same_request!("q", "s"),
    " RETURNING 1) SELECT count(*) FROM gone"
);

const APPROVALS: &str = "DELETE FROM alt_current_approvals WHERE (repository,epoch,actor_id,request_id) IN (SELECT repository,epoch,actor_id,request_id FROM alt_current_approvals WHERE expires_at<=$1 LIMIT $2)";

/// rows of `table`, which stand on a credential, whose credential ended before `$1`
macro_rules! on_a_credential_that_ended {
    ($table:literal, $also:literal) => {
        concat!(
            "DELETE FROM ",
            $table,
            " WHERE token_digest IN (SELECT t.token_digest FROM ",
            $table,
            " t JOIN alt_credentials c USING(token_digest) WHERE c.expires_at<=$1 OR c.revoked_at<=$1",
            $also,
            " LIMIT $2)"
        )
    };
}
const WEB_SESSIONS: &str = on_a_credential_that_ended!("alt_web_sessions", " OR t.expires_at<=$1");
const CLIENT_CREDENTIALS: &str = on_a_credential_that_ended!("alt_client_credentials", "");
const CLOUD_CREDENTIALS: &str = on_a_credential_that_ended!("alt_cloud_credentials", "");
const DELEGATIONS: &str = on_a_credential_that_ended!("alt_delegations", "");

const DEVICE_AUTHORIZATIONS: &str = "DELETE FROM alt_device_authorizations WHERE device_digest IN (SELECT device_digest FROM alt_device_authorizations WHERE expires_at<=$1 LIMIT $2)";
const CLOUD_LOGINS: &str = "DELETE FROM alt_cloud_logins WHERE id IN (SELECT id FROM alt_cloud_logins WHERE expires_at<=$1 LIMIT $2)";

/// `$1` is the moment before which a credential ended long enough ago, `$2` now
const CREDENTIALS: &str = "DELETE FROM alt_credentials WHERE token_digest IN (SELECT c.token_digest FROM alt_credentials c WHERE (c.expires_at<=$1 OR (c.revoked_at<=$1 AND c.expires_at<=$2)) AND NOT EXISTS(SELECT 1 FROM alt_web_sessions x WHERE x.token_digest=c.token_digest) AND NOT EXISTS(SELECT 1 FROM alt_client_credentials x WHERE x.token_digest=c.token_digest) AND NOT EXISTS(SELECT 1 FROM alt_cloud_credentials x WHERE x.token_digest=c.token_digest) AND NOT EXISTS(SELECT 1 FROM alt_delegations x WHERE x.token_digest=c.token_digest) AND NOT EXISTS(SELECT 1 FROM alt_delegations x WHERE x.parent_digest=c.token_digest) AND NOT EXISTS(SELECT 1 FROM alt_device_authorizations x WHERE x.approved_by=c.token_digest) AND NOT EXISTS(SELECT 1 FROM alt_current_approvals x WHERE x.reviewer_credential=c.token_digest) LIMIT $3)";

impl Store {
    /// One round of removing what is over, up to `limit` rows by each rule. A
    /// rule that fails is told to `failed` by its name and the others go on.
    /// `seen` is kept by the caller from one round to the next.
    pub fn remove_what_is_over(
        &mut self,
        limit: u32,
        seen: &mut RequestsSeen,
        failed: &mut dyn FnMut(&str, Error),
    ) -> Removed {
        let limit = i64::from(limit);
        let mut more = false;
        let mut told = |name: &str, result: Result<(u64, bool)>| match result {
            Ok((count, full)) => {
                more |= full;
                count
            }
            Err(error) => {
                failed(name, error);
                0
            }
        };
        let sessions = SESSION_RETENTION_MS;
        let removed = Removed {
            requests: told("requests", self.requests_over(limit, seen)),
            unfinished_requests: told("unfinished requests", self.unfinished_over(limit)),
            approvals: told("approvals", self.rule(APPROVALS, limit, sessions)),
            web_sessions: told("web sessions", self.rule(WEB_SESSIONS, limit, sessions)),
            client_credentials: told(
                "client credentials",
                self.rule(CLIENT_CREDENTIALS, limit, sessions),
            ),
            cloud_credentials: told(
                "cloud credentials",
                self.rule(CLOUD_CREDENTIALS, limit, sessions),
            ),
            delegations: told("delegations", self.rule(DELEGATIONS, limit, sessions)),
            device_authorizations: told(
                "device authorizations",
                self.rule(DEVICE_AUTHORIZATIONS, limit, 0),
            ),
            cloud_logins: told("cloud logins", self.rule(CLOUD_LOGINS, limit, 0)),
            credentials: told("credentials", self.credentials_over(limit)),
            more: false,
        };
        Removed { more, ..removed }
    }

    fn removing(&mut self) -> Result<(Transaction<'_>, u64)> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let time = now(&mut tx)?;
        Ok((tx, time))
    }

    /// a rule whose statement takes the moment before which a row goes, and the limit
    fn rule(&mut self, statement: &str, limit: i64, kept_ms: u64) -> Result<(u64, bool)> {
        let (mut tx, time) = self.removing()?;
        let before = integer(time.saturating_sub(kept_ms))?;
        let count = tx.execute(statement, &[&before, &limit])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok((count, count >= limit as u64))
    }

    fn unfinished_over(&mut self, limit: i64) -> Result<(u64, bool)> {
        let (mut tx, time) = self.removing()?;
        let before = integer(time.saturating_sub(REQUEST_RETENTION_MS))?;
        let count = unsigned(tx.query_one(UNFINISHED, &[&before, &limit])?.get(0))?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok((count, count >= limit as u64))
    }

    fn requests_over(&mut self, limit: i64, seen: &mut RequestsSeen) -> Result<(u64, bool)> {
        let (mut tx, time) = self.removing()?;
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
            return Ok((0, false));
        };
        *seen = RequestsSeen {
            settled_at: row.get(0),
            repository: row.get(1),
            epoch: row.get(2),
            actor: row.get(3),
            request: row.get(4),
        };
        Ok((unsigned(row.get(5))?, row.get::<_, i64>(6) >= limit))
    }

    fn credentials_over(&mut self, limit: i64) -> Result<(u64, bool)> {
        let (mut tx, time) = self.removing()?;
        let before = integer(time.saturating_sub(CREDENTIAL_RETENTION_MS))?;
        let count = tx.execute(CREDENTIALS, &[&before, &integer(time)?, &limit])?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Ok((count, count >= limit as u64))
    }
}
