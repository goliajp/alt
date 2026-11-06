-- Cloud authority is separate from local accounts. Credentials remain digests;
-- upstream grants are authenticated ciphertext produced by the HTTP adapter.
CREATE TABLE alt_cloud_binding (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  audience jsonb NOT NULL,
  runtime_key_digest text NOT NULL,
  provision_digest text CHECK (provision_digest ~ '^[a-f0-9]{64}$'),
  clock_high_water bigint NOT NULL DEFAULT 0 CHECK (clock_high_water >= 0),
  authority_revision bigint NOT NULL DEFAULT 0 CHECK (authority_revision >= 0),
  service jsonb,
  service_denied_at bigint NOT NULL DEFAULT 0 CHECK (service_denied_at >= 0)
);
CREATE TABLE alt_cloud_accounts (
  actor_id text PRIMARY KEY,
  membership_id text NOT NULL
);
CREATE TABLE alt_cloud_credentials (
  token_digest text PRIMARY KEY REFERENCES alt_credentials(token_digest),
  claims jsonb NOT NULL,
  sealed_grant text NOT NULL,
  created_at bigint NOT NULL CHECK (created_at >= 0),
  browser boolean NOT NULL
);
CREATE TABLE alt_cloud_ingress (
  month text PRIMARY KEY,
  accepted_bytes bigint NOT NULL DEFAULT 0 CHECK (accepted_bytes >= 0),
  reserved_bytes bigint NOT NULL DEFAULT 0 CHECK (reserved_bytes >= 0)
);
CREATE TABLE alt_cloud_reservations (
  repository text NOT NULL,
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  month text NOT NULL REFERENCES alt_cloud_ingress(month),
  remaining_bytes bigint NOT NULL CHECK (remaining_bytes >= 0),
  PRIMARY KEY(repository,epoch,actor_id,request_id),
  FOREIGN KEY(repository,epoch,actor_id,request_id)
    REFERENCES alt_requests(repository,epoch,actor_id,request_id)
);
CREATE TABLE alt_delegations (
  token_digest text PRIMARY KEY REFERENCES alt_credentials(token_digest),
  parent_digest text NOT NULL REFERENCES alt_credentials(token_digest),
  scope jsonb NOT NULL,
  purpose text NOT NULL,
  delegation_id text NOT NULL,
  created_at bigint NOT NULL,
  UNIQUE(parent_digest,delegation_id),
  CHECK (token_digest <> parent_digest)
);

-- Browser login stores only derived identifiers. A claimed remote exchange is
-- never repeated after an indeterminate outcome; a committed local login can
-- return the same derived credential after a lost HTTP response.
CREATE TABLE alt_cloud_logins (
  id text PRIMARY KEY,
  credential_digest text NOT NULL UNIQUE,
  credential_expires_at bigint NOT NULL,
  expires_at bigint NOT NULL,
  state text NOT NULL CHECK (state IN ('pending','exchanging','complete','refused')),
  ticket_digest text
);

-- Standard device authorization is shared by both deployment modes. Only code
-- digests persist; approval atomically creates a separately revocable credential.
CREATE TABLE alt_device_authorizations (
  device_digest text PRIMARY KEY,
  user_digest text NOT NULL UNIQUE,
  credential_digest text NOT NULL UNIQUE,
  state text NOT NULL CHECK(state IN ('pending','approved','denied')),
  approved_by text REFERENCES alt_credentials(token_digest),
  created_at bigint NOT NULL,
  expires_at bigint NOT NULL,
  next_poll_at bigint NOT NULL,
  interval_ms bigint NOT NULL CHECK(interval_ms >= 5000)
);
CREATE TABLE alt_client_credentials (
  token_digest text PRIMARY KEY REFERENCES alt_credentials(token_digest),
  client_id text NOT NULL,
  created_at bigint NOT NULL
);
