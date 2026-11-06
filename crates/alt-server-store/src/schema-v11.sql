-- Canonical signed cross-product history is logical repository content.
-- Live channels, credentials, queues and claims are not portable history.
ALTER TABLE alt_repositories ADD COLUMN link_bytes bigint NOT NULL DEFAULT 0
  CHECK(link_bytes >= 0);
CREATE TABLE alt_link_history (
  repository text NOT NULL REFERENCES alt_repositories(id),
  event_digest text NOT NULL CHECK(length(event_digest)=64),
  history bytea NOT NULL CHECK(octet_length(history) BETWEEN 1 AND 131072),
  PRIMARY KEY(repository,event_digest)
);
