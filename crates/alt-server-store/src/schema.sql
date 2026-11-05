CREATE TABLE alt_instance (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  installation_id text NOT NULL,
  workspace_id text NOT NULL,
  deployment_mode text NOT NULL CHECK (deployment_mode IN ('self_hosted','cloud')),
  runtime_generation bigint NOT NULL CHECK (runtime_generation > 0),
  signing_key_id text NOT NULL
);
CREATE TABLE alt_repositories (
  id text PRIMARY KEY,
  recovery_epoch bigint NOT NULL CHECK (recovery_epoch > 0),
  object_format text NOT NULL CHECK (object_format = 'sha1'),
  transaction_sequence bigint NOT NULL DEFAULT 0 CHECK (transaction_sequence >= 0),
  policy_version bigint NOT NULL DEFAULT 1 CHECK (policy_version > 0),
  revocation_generation bigint NOT NULL DEFAULT 1 CHECK (revocation_generation > 0),
  policies jsonb NOT NULL DEFAULT '[]',
  writes_enabled boolean NOT NULL DEFAULT true,
  quota_bytes bigint NOT NULL CHECK (quota_bytes >= 0),
  stored_bytes bigint NOT NULL DEFAULT 0 CHECK (stored_bytes >= 0),
  reserved_bytes bigint NOT NULL DEFAULT 0 CHECK (reserved_bytes >= 0)
);
CREATE TABLE alt_credentials (
  token_digest text PRIMARY KEY,
  actor_id text NOT NULL,
  attribution jsonb NOT NULL,
  identity_source text NOT NULL CHECK (identity_source IN ('self_hosted','cloud')),
  expires_at bigint NOT NULL,
  revoked boolean NOT NULL DEFAULT false,
  administrator boolean NOT NULL DEFAULT false
);
CREATE TABLE alt_grants (
  actor_id text NOT NULL,
  repository text NOT NULL REFERENCES alt_repositories(id),
  can_read boolean NOT NULL,
  publish_records boolean NOT NULL,
  grants jsonb NOT NULL,
  PRIMARY KEY(actor_id, repository)
);
CREATE TABLE alt_refs (
  repository text NOT NULL REFERENCES alt_repositories(id),
  name text NOT NULL,
  oid text NOT NULL,
  PRIMARY KEY(repository, name)
);
CREATE TABLE alt_requests (
  repository text NOT NULL REFERENCES alt_repositories(id),
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  digest text NOT NULL,
  publication jsonb NOT NULL,
  manifest jsonb NOT NULL,
  state text NOT NULL CHECK (state IN ('receiving','ready','committed','rejected','expired')),
  result jsonb NOT NULL,
  expires_at bigint NOT NULL,
  reserved_bytes bigint NOT NULL CHECK (reserved_bytes >= 0),
  fence bigint NOT NULL DEFAULT 0 CHECK (fence >= 0),
  worker_lease_until bigint NOT NULL DEFAULT 0,
  PRIMARY KEY(repository, epoch, actor_id, request_id)
);
CREATE TABLE alt_blobs (
  generation bigint GENERATED ALWAYS AS IDENTITY,
  repository text NOT NULL REFERENCES alt_repositories(id),
  blake3 text NOT NULL,
  bytes bigint NOT NULL CHECK (bytes >= 0),
  state text NOT NULL CHECK (state IN ('staging','stored','deleting')),
  accounted boolean NOT NULL DEFAULT false,
  created_at bigint NOT NULL,
  PRIMARY KEY(repository, blake3)
);
CREATE TABLE alt_request_content (
  repository text NOT NULL,
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  blake3 text NOT NULL,
  verified boolean NOT NULL,
  PRIMARY KEY(repository,epoch,actor_id,request_id,blake3),
  FOREIGN KEY(repository,epoch,actor_id,request_id) REFERENCES alt_requests(repository,epoch,actor_id,request_id),
  FOREIGN KEY(repository,blake3) REFERENCES alt_blobs(repository,blake3) ON DELETE CASCADE
);
CREATE TABLE alt_objects (
  repository text NOT NULL,
  oid text NOT NULL,
  kind text NOT NULL CHECK (kind IN ('blob','tree','commit','tag')),
  blake3 text NOT NULL,
  PRIMARY KEY(repository, oid),
  FOREIGN KEY(repository, blake3) REFERENCES alt_blobs(repository, blake3)
);
-- Every pin is scoped to this repository; opaque pin IDs never grant access.
-- expires_at=NULL is a durable publication root; temporary read/upload/backup
-- pins expire under the same repository lock used by publication and GC.
CREATE TABLE alt_pins (
  repository text NOT NULL,
  pin_id text NOT NULL,
  blake3 text NOT NULL,
  expires_at bigint,
  PRIMARY KEY(repository, pin_id, blake3),
  FOREIGN KEY(repository, blake3) REFERENCES alt_blobs(repository, blake3)
);
CREATE INDEX alt_pins_expiry ON alt_pins(repository, expires_at);
CREATE TABLE alt_task_roots (
  repository text NOT NULL REFERENCES alt_repositories(id),
  kind text NOT NULL,
  record_id text NOT NULL,
  publication_digest text NOT NULL,
  PRIMARY KEY(repository, kind, record_id)
);
CREATE TABLE alt_audit (
  repository text NOT NULL REFERENCES alt_repositories(id),
  sequence bigint NOT NULL,
  actor_id text NOT NULL,
  event jsonb NOT NULL,
  PRIMARY KEY(repository, sequence)
);
CREATE TABLE alt_outbox (
  repository text NOT NULL,
  sequence bigint NOT NULL,
  delivered boolean NOT NULL DEFAULT false,
  attempts bigint NOT NULL DEFAULT 0,
  next_attempt_at bigint NOT NULL DEFAULT 0,
  event jsonb NOT NULL,
  PRIMARY KEY(repository, sequence),
  FOREIGN KEY(repository, sequence) REFERENCES alt_audit(repository, sequence)
);
