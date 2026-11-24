-- What is over is removed after a time, counted from when it ended: a request
-- from when it was committed, rejected or expired, a credential from when it was
-- revoked. A request that ended before this version has no such moment recorded,
-- and is given none here: writing every row of the table in one transaction
-- leaves a second copy of the table behind. The server gives each its moment as
-- it runs, so many rows at a time, and a request without one is kept until then.
-- alt_requests_unsettled holds the ones still to be given theirs, and nothing
-- once that is done.
ALTER TABLE alt_requests ADD COLUMN settled_at bigint;
ALTER TABLE alt_requests ADD CONSTRAINT alt_requests_settled_at CHECK (settled_at IS NULL OR state IN ('committed','rejected','expired'));
CREATE INDEX alt_requests_settled ON alt_requests(settled_at) WHERE settled_at IS NOT NULL;
CREATE INDEX alt_requests_unsettled ON alt_requests(expires_at) WHERE settled_at IS NULL AND state IN ('committed','rejected','expired');
-- the requests still open are few among all there ever were, and are looked for often
CREATE INDEX alt_requests_open ON alt_requests(expires_at) WHERE state IN ('receiving','ready');
ALTER TABLE alt_credentials ADD COLUMN revoked_at bigint;
UPDATE alt_credentials SET revoked_at=floor(extract(epoch FROM clock_timestamp())*1000)::bigint WHERE revoked;
ALTER TABLE alt_credentials ADD CONSTRAINT alt_credentials_revoked_at CHECK ((revoked_at IS NOT NULL) = revoked);
-- a request is removed only when no segment names it, which is asked for each one
DROP INDEX alt_segments_owner;
CREATE INDEX alt_segments_request ON alt_segments(repository,epoch,actor_id,request_id);
