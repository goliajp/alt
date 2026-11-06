-- Operational claim is made only inside a verified native publication.
-- It contains no Task bytes, bearer token or execution assertion. Historical
-- Task/RepairSource records remain exportable independently of this index.
CREATE TABLE alt_link_task_claims (
  repository text NOT NULL REFERENCES alt_repositories(id),
  epoch bigint NOT NULL CHECK(epoch>0),
  channel text NOT NULL,
  entity text NOT NULL,
  repair_digest text NOT NULL,
  task_id text NOT NULL,
  publication_digest text NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  PRIMARY KEY(repository,epoch,channel,entity),
  UNIQUE(repository,epoch,repair_digest),
  FOREIGN KEY(repository,repair_digest) REFERENCES alt_link_history(repository,event_digest)
);
