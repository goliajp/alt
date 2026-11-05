-- Raw envelopes remain immutable byte objects; multiple original encodings of
-- one canonical record ID retain their separate content identities.
CREATE TABLE alt_task_records (
  repository text NOT NULL,
  kind text NOT NULL,
  record_id text NOT NULL,
  payload_blake3 text NOT NULL,
  PRIMARY KEY(repository,kind,record_id,payload_blake3),
  FOREIGN KEY(repository,payload_blake3) REFERENCES alt_blobs(repository,blake3)
);
CREATE TABLE alt_task_bundles (
  repository text NOT NULL,
  bundle_blake3 text NOT NULL,
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  PRIMARY KEY(repository,epoch,actor_id,request_id),
  FOREIGN KEY(repository,bundle_blake3) REFERENCES alt_blobs(repository,blake3),
  FOREIGN KEY(repository,epoch,actor_id,request_id) REFERENCES alt_requests(repository,epoch,actor_id,request_id)
);
CREATE TABLE alt_collector_policies (
  repository text PRIMARY KEY REFERENCES alt_repositories(id),
  generation bigint NOT NULL CHECK(generation>0),
  policy jsonb NOT NULL
);
CREATE TABLE alt_review_grants (
  repository text NOT NULL REFERENCES alt_repositories(id),
  actor_id text NOT NULL,
  can_review boolean NOT NULL,
  PRIMARY KEY(repository,actor_id)
);
CREATE TABLE alt_current_approvals (
  repository text NOT NULL,
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  request_digest text NOT NULL,
  bundle_blake3 text NOT NULL,
  integration_id text NOT NULL,
  review_id text NOT NULL,
  reviewer text NOT NULL,
  reviewer_credential text NOT NULL REFERENCES alt_credentials(token_digest),
  policy_version bigint NOT NULL,
  collector_generation bigint NOT NULL,
  revocation_generation bigint NOT NULL,
  approved_at bigint NOT NULL,
  expires_at bigint NOT NULL,
  consumed_sequence bigint,
  PRIMARY KEY(repository,epoch,actor_id,request_id),
  FOREIGN KEY(repository,epoch,actor_id,request_id) REFERENCES alt_requests(repository,epoch,actor_id,request_id)
);
