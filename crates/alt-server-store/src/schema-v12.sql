-- Operational bindings are deliberately absent from logical portable catalogs.
-- Recovery epoch fencing leaves restored old bindings inert.
CREATE TABLE alt_link_channels (
  repository text NOT NULL REFERENCES alt_repositories(id),
  epoch bigint NOT NULL CHECK(epoch>0),
  channel text NOT NULL,
  specification jsonb NOT NULL,
  enabled boolean NOT NULL,
  created_at bigint NOT NULL,
  disabled_at bigint,
  PRIMARY KEY(repository,epoch,channel)
);
-- These rows point to canonical history; actor credentials are opaque digests,
-- never bearer tokens. Configured peer URL/secret files stay in operator config.
CREATE TABLE alt_link_outbox (
  repository text NOT NULL,
  epoch bigint NOT NULL CHECK(epoch>0),
  channel text NOT NULL,
  configuration_digest text NOT NULL,
  entity text NOT NULL,
  revision bigint NOT NULL CHECK(revision>0),
  event_digest text NOT NULL,
  actor_id text NOT NULL,
  author_credential text NOT NULL,
  request_id text NOT NULL,
  intent_digest text NOT NULL,
  receipt_reservation bigint NOT NULL CHECK(receipt_reservation>=0),
  created_at bigint NOT NULL,
  delivered_at bigint,
  PRIMARY KEY(repository,epoch,channel,entity,revision),
  UNIQUE(repository,epoch,actor_id,request_id),
  FOREIGN KEY(repository,event_digest) REFERENCES alt_link_history(repository,event_digest)
);
CREATE INDEX alt_link_pending ON alt_link_outbox(repository,epoch,created_at) WHERE delivered_at IS NULL;
CREATE TABLE alt_link_inbox (
  repository text NOT NULL,
  epoch bigint NOT NULL CHECK(epoch>0),
  channel text NOT NULL,
  configuration_digest text NOT NULL,
  entity text NOT NULL,
  revision bigint NOT NULL CHECK(revision>0),
  event_digest text NOT NULL,
  received_at bigint NOT NULL,
  PRIMARY KEY(repository,epoch,channel,entity,revision),
  FOREIGN KEY(repository,event_digest) REFERENCES alt_link_history(repository,event_digest)
);
CREATE TABLE alt_link_current (
  repository text NOT NULL,
  epoch bigint NOT NULL CHECK(epoch>0),
  channel text NOT NULL,
  entity text NOT NULL,
  kind text NOT NULL CHECK(kind IN ('mapping','withdraw','repair_request','task_bound','observation')),
  event_digest text NOT NULL,
  PRIMARY KEY(repository,epoch,channel,entity),
  FOREIGN KEY(repository,event_digest) REFERENCES alt_link_history(repository,event_digest)
);
