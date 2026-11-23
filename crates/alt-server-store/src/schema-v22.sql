-- What is over is removed after a time, counted from when it ended: a request
-- from when it was committed, rejected or expired, a credential from when it was
-- revoked. Rows that ended before this version have no such moment recorded: a
-- committed request has the one in its receipt, and the others are given the
-- moment of the upgrade, so nothing is removed sooner than it would have been.
ALTER TABLE alt_requests ADD COLUMN settled_at bigint;
UPDATE alt_requests SET settled_at=CASE WHEN state='committed' THEN (result#>>'{receipt,body,committed_at_unix_ms}')::bigint ELSE floor(extract(epoch FROM clock_timestamp())*1000)::bigint END WHERE state IN ('committed','rejected','expired');
ALTER TABLE alt_requests ADD CONSTRAINT alt_requests_settled_at CHECK ((settled_at IS NOT NULL) = (state IN ('committed','rejected','expired')));
CREATE INDEX alt_requests_settled ON alt_requests(settled_at) WHERE settled_at IS NOT NULL;
-- the requests still open are few among all there ever were, and are looked for often
CREATE INDEX alt_requests_open ON alt_requests(expires_at) WHERE state IN ('receiving','ready');
ALTER TABLE alt_credentials ADD COLUMN revoked_at bigint;
UPDATE alt_credentials SET revoked_at=floor(extract(epoch FROM clock_timestamp())*1000)::bigint WHERE revoked;
ALTER TABLE alt_credentials ADD CONSTRAINT alt_credentials_revoked_at CHECK ((revoked_at IS NOT NULL) = revoked);
-- a request is removed only when no segment names it, which is asked for each one
DROP INDEX alt_segments_owner;
CREATE INDEX alt_segments_request ON alt_segments(repository,epoch,actor_id,request_id);
