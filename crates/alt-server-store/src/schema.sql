-- CARVE-OUT: the whole layout is one file, held to one checksum
-- The current schema, for a database that is being created. An existing database
-- reaches it through the upgrade from the schema before it, never through this file.
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
  reserved_bytes bigint NOT NULL DEFAULT 0 CHECK (reserved_bytes >= 0),
  -- HEAD is repository metadata, not a heuristic over whatever ref happens to
  -- sort first. Operators may select an unborn branch explicitly.
  default_branch text NOT NULL DEFAULT 'refs/heads/main',
  -- A cutover freeze is independent of service entitlement and operator write
  -- settings. Releasing it never turns either of those permissions on.
  portable_freeze text,
  portable_import text,
  -- Canonical signed cross-product history is logical repository content.
  -- Live channels, credentials, queues and claims are not portable history.
  link_bytes bigint NOT NULL DEFAULT 0 CHECK(link_bytes >= 0),
  -- set once a ref was deleted or moved off its history: some stored objects may
  -- no longer be reachable, so whole segments are not resent until compaction
  pack_unreachable boolean NOT NULL DEFAULT false
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
-- The result of a committed request is its receipt without the publication, which
-- the row holds once in its own column; the outbox event of the publication is the
-- same. The publication documents are compressed with lz4.
CREATE TABLE alt_requests (
  repository text NOT NULL REFERENCES alt_repositories(id),
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  digest text NOT NULL,
  publication jsonb COMPRESSION lz4 NOT NULL,
  manifest jsonb NOT NULL,
  state text NOT NULL CHECK (state IN ('receiving','ready','committed','rejected','expired')),
  result jsonb COMPRESSION lz4 NOT NULL,
  expires_at bigint NOT NULL,
  reserved_bytes bigint NOT NULL CHECK (reserved_bytes >= 0),
  fence bigint NOT NULL DEFAULT 0 CHECK (fence >= 0),
  worker_lease_until bigint NOT NULL DEFAULT 0,
  PRIMARY KEY(repository, epoch, actor_id, request_id)
);
-- State/verified updates of alt_blobs and alt_request_content do not change
-- indexed keys. Their fillfactor reserves heap-page room for HOT updates rather
-- than rewriting every index entry.
CREATE TABLE alt_blobs (
  generation bigint GENERATED ALWAYS AS IDENTITY,
  repository text NOT NULL REFERENCES alt_repositories(id),
  blake3 text NOT NULL,
  bytes bigint NOT NULL CHECK (bytes >= 0),
  state text NOT NULL CHECK (state IN ('staging','stored','deleting')),
  accounted boolean NOT NULL DEFAULT false,
  created_at bigint NOT NULL,
  PRIMARY KEY(repository, blake3)
) WITH (fillfactor=50);
CREATE TABLE alt_request_content (
  repository text NOT NULL,
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  blake3 text NOT NULL,
  verified boolean NOT NULL,
  -- Indexed projections of an immutable intent. The original manifest remains
  -- authoritative history; progress and these selections share its request key.
  selected_entries jsonb NOT NULL DEFAULT '[]'::jsonb,
  chunk_bytes bigint,
  PRIMARY KEY(repository,epoch,actor_id,request_id,blake3),
  FOREIGN KEY(repository,epoch,actor_id,request_id) REFERENCES alt_requests(repository,epoch,actor_id,request_id),
  FOREIGN KEY(repository,blake3) REFERENCES alt_blobs(repository,blake3) ON DELETE CASCADE,
  CONSTRAINT alt_upload_selection_array CHECK (jsonb_typeof(selected_entries)='array'),
  CONSTRAINT alt_upload_selection_present CHECK (selected_entries<>'[]'::jsonb OR chunk_bytes IS NOT NULL),
  CONSTRAINT alt_upload_selection_chunk_size CHECK (chunk_bytes>0)
) WITH (fillfactor=50);
CREATE TABLE alt_objects (
  repository text NOT NULL,
  oid text NOT NULL,
  kind text NOT NULL CHECK (kind IN ('blob','tree','commit','tag')),
  blake3 text NOT NULL,
  PRIMARY KEY(repository, oid),
  FOREIGN KEY(repository, blake3) REFERENCES alt_blobs(repository, blake3)
);
-- Every pin is scoped to this repository; opaque pin IDs never grant access.
-- expires_at=NULL keeps content for good; temporary read/upload/backup pins
-- expire under the same repository lock used by publication and GC. Published
-- Git objects live in the pack segments alone, and refs record no retention in
-- the content store.
CREATE TABLE alt_pins (
  repository text NOT NULL,
  pin_id text NOT NULL,
  blake3 text NOT NULL,
  expires_at bigint,
  PRIMARY KEY(repository, pin_id, blake3),
  FOREIGN KEY(repository, blake3) REFERENCES alt_blobs(repository, blake3),
  CONSTRAINT alt_pins_no_ref CHECK (pin_id NOT LIKE 'ref:%')
);
CREATE INDEX alt_pins_expiry ON alt_pins(repository, expires_at);
-- Permanent holders are selected by repository and digest, independently of
-- their pin identity. Multiple permanent holders remain valid.
CREATE INDEX alt_pins_permanent_content ON alt_pins(repository,blake3)
    WHERE expires_at IS NULL;
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
  event jsonb COMPRESSION lz4 NOT NULL,
  PRIMARY KEY(repository, sequence)
);
CREATE TABLE alt_outbox (
  repository text NOT NULL,
  sequence bigint NOT NULL,
  delivered boolean NOT NULL DEFAULT false,
  attempts bigint NOT NULL DEFAULT 0,
  next_attempt_at bigint NOT NULL DEFAULT 0,
  event jsonb COMPRESSION lz4 NOT NULL,
  PRIMARY KEY(repository, sequence),
  FOREIGN KEY(repository, sequence) REFERENCES alt_audit(repository, sequence)
);
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
-- A root may be explicitly republished with another complete selection or
-- attachment disposition. Keep every publication association, not only the
-- first appearance in the distinct-root catalog.
CREATE TABLE alt_task_bundle_roots (
  repository text NOT NULL,
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  kind text NOT NULL,
  record_id text NOT NULL,
  PRIMARY KEY(repository,epoch,actor_id,request_id,kind,record_id),
  FOREIGN KEY(repository,epoch,actor_id,request_id) REFERENCES alt_task_bundles(repository,epoch,actor_id,request_id)
);
CREATE INDEX alt_task_bundle_roots_lookup ON alt_task_bundle_roots(repository,kind,record_id);

-- Selected dependency records must also be discoverable (for example a Task
-- received as part of an Integration root). The association binds its exact
-- original payload variant to a particular published selection.
CREATE TABLE alt_task_bundle_records (
  repository text NOT NULL,
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  kind text NOT NULL,
  record_id text NOT NULL,
  payload_blake3 text NOT NULL,
  PRIMARY KEY(repository,epoch,actor_id,request_id,kind,record_id),
  FOREIGN KEY(repository,epoch,actor_id,request_id) REFERENCES alt_task_bundles(repository,epoch,actor_id,request_id),
  FOREIGN KEY(repository,kind,record_id,payload_blake3) REFERENCES alt_task_records(repository,kind,record_id,payload_blake3)
);
CREATE INDEX alt_task_bundle_records_lookup ON alt_task_bundle_records(repository,kind,record_id);
CREATE TABLE alt_browser_binding (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  public_origin text NOT NULL
);
CREATE TABLE alt_web_accounts (
  actor_id text PRIMARY KEY,
  login_name text UNIQUE NOT NULL,
  display_name text NOT NULL,
  password_hash text NOT NULL,
  administrator boolean NOT NULL,
  disabled boolean NOT NULL DEFAULT false,
  generation bigint NOT NULL DEFAULT 1 CHECK (generation > 0)
);
CREATE TABLE alt_web_sessions (
  token_digest text PRIMARY KEY REFERENCES alt_credentials(token_digest),
  actor_id text NOT NULL REFERENCES alt_web_accounts(actor_id),
  account_generation bigint NOT NULL CHECK (account_generation > 0),
  expires_at bigint NOT NULL,
  created_at bigint NOT NULL
);
CREATE INDEX alt_web_sessions_actor ON alt_web_sessions(actor_id, expires_at);
CREATE TABLE alt_login_budgets (
  key text PRIMARY KEY,
  window_start bigint NOT NULL,
  attempts integer NOT NULL CHECK (attempts > 0)
);
CREATE TABLE alt_instance_audit (
  sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  actor_id text NOT NULL,
  action text NOT NULL,
  subject_id text NOT NULL,
  details jsonb NOT NULL,
  occurred_at bigint NOT NULL
);
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
-- Original requests and receipts remain unchanged. This lineage connects their
-- historical authority to the current repository scope after explicit transfer.
CREATE TABLE alt_publication_provenance (
  repository text NOT NULL REFERENCES alt_repositories(id),
  publication_digest text NOT NULL,
  proofs jsonb NOT NULL CHECK (jsonb_typeof(proofs) = 'array' AND jsonb_array_length(proofs) BETWEEN 1 AND 32),
  PRIMARY KEY(repository, publication_digest)
);
-- Logical migration snapshots contain an explicit allowlist, never database
-- credentials, sessions or receipt signing secrets. Export IDs are immutable.
CREATE TABLE alt_portable_exports (
  repository text NOT NULL REFERENCES alt_repositories(id),
  export_id text NOT NULL,
  actor_id text NOT NULL,
  envelope jsonb NOT NULL,
  documents jsonb NOT NULL,
  retired boolean NOT NULL DEFAULT false,
  created_at bigint NOT NULL,
  -- Snapshot intent and retention become durable before expensive validation.
  -- Only a completed validation can expose the signed envelope for download.
  verified_at bigint CHECK(verified_at>=0),
  PRIMARY KEY(repository,export_id)
);
-- Portable intents are distinct from native publication requests. Incomplete
-- imported bytes remain private temporary roots until atomic activation.
CREATE TABLE alt_portable_imports (
  repository text NOT NULL REFERENCES alt_repositories(id),
  import_id text NOT NULL,
  actor_id text NOT NULL,
  epoch bigint NOT NULL CHECK(epoch>0),
  digest text NOT NULL,
  intent jsonb NOT NULL,
  envelope jsonb NOT NULL,
  state text NOT NULL CHECK(state IN ('receiving','committed','abandoned')),
  receipt jsonb,
  expires_at bigint NOT NULL,
  reserved_bytes bigint NOT NULL CHECK(reserved_bytes>=0),
  created_at bigint NOT NULL,
  PRIMARY KEY(repository,import_id),
  CHECK((state='committed')=(receipt IS NOT NULL))
);
CREATE TABLE alt_portable_import_content (
  repository text NOT NULL,
  import_id text NOT NULL,
  blake3 text NOT NULL,
  verified boolean NOT NULL DEFAULT false,
  PRIMARY KEY(repository,import_id,blake3),
  FOREIGN KEY(repository,import_id) REFERENCES alt_portable_imports(repository,import_id),
  FOREIGN KEY(repository,blake3) REFERENCES alt_blobs(repository,blake3) ON DELETE CASCADE
);
CREATE TABLE alt_cloud_portable_reservations (
  repository text NOT NULL,
  import_id text NOT NULL,
  month text NOT NULL REFERENCES alt_cloud_ingress(month),
  remaining_bytes bigint NOT NULL CHECK(remaining_bytes>=0),
  PRIMARY KEY(repository,import_id),
  FOREIGN KEY(repository,import_id) REFERENCES alt_portable_imports(repository,import_id)
);
-- Imported history is not a native request executed by this installation.
-- Preserve query projections and exact original metadata byte descriptors.
CREATE TABLE alt_archived_publications (
  repository text NOT NULL REFERENCES alt_repositories(id),
  digest text NOT NULL,
  publication jsonb NOT NULL,
  manifest jsonb NOT NULL,
  receipt jsonb NOT NULL,
  original jsonb NOT NULL,
  PRIMARY KEY(repository,digest)
);
CREATE TABLE alt_archived_records (
  repository text NOT NULL,
  digest text NOT NULL,
  kind text NOT NULL,
  record_id text NOT NULL,
  payload_blake3 text NOT NULL,
  PRIMARY KEY(repository,digest,kind,record_id),
  FOREIGN KEY(repository,digest) REFERENCES alt_archived_publications(repository,digest),
  FOREIGN KEY(repository,kind,record_id,payload_blake3) REFERENCES alt_task_records(repository,kind,record_id,payload_blake3)
);
CREATE INDEX alt_archived_records_lookup ON alt_archived_records(repository,kind,record_id);
CREATE TABLE alt_link_history (
  repository text NOT NULL REFERENCES alt_repositories(id),
  event_digest text NOT NULL CHECK(length(event_digest)=64),
  history bytea NOT NULL CHECK(octet_length(history) BETWEEN 1 AND 131072),
  PRIMARY KEY(repository,event_digest)
);
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
-- Physical locations are subordinate to the existing logical blob catalog.
-- A committed bounded intent owns files even if installation/registration dies.
CREATE TABLE alt_segments (
    repository text NOT NULL REFERENCES alt_repositories(id),
    segment_id text NOT NULL CHECK (segment_id ~ '^[0-9a-f]{32}$'),
    owner_kind text NOT NULL CHECK (owner_kind IN ('upload','compact')),
    epoch bigint,
    actor_id text,
    request_id text,
    state text NOT NULL CHECK (state IN ('uploading','stored','deleting')),
    expires_at bigint NOT NULL CHECK (expires_at > 0),
    member_count bigint NOT NULL CHECK (member_count BETWEEN 1 AND 8192),
    payload_bytes bigint NOT NULL CHECK (payload_bytes BETWEEN 0 AND 67108864),
    selection jsonb NOT NULL,
    index_digest text CHECK (index_digest ~ '^[0-9a-f]{64}$'),
    PRIMARY KEY(repository,segment_id),
    FOREIGN KEY(repository,epoch,actor_id,request_id)
      REFERENCES alt_requests(repository,epoch,actor_id,request_id),
    CHECK ((owner_kind='upload' AND epoch IS NOT NULL AND actor_id IS NOT NULL AND request_id IS NOT NULL)
        OR (owner_kind='compact' AND epoch IS NULL AND actor_id IS NULL AND request_id IS NULL)),
    CHECK (state<>'stored' OR index_digest IS NOT NULL)
);
CREATE INDEX alt_segments_owner ON alt_segments(repository,epoch,actor_id,request_id) WHERE state='uploading';
CREATE TABLE alt_blob_locations (
    repository text NOT NULL,
    blake3 text NOT NULL,
    segment_id text NOT NULL,
    index_digest text NOT NULL CHECK (index_digest ~ '^[0-9a-f]{64}$'),
    byte_offset bigint NOT NULL CHECK (byte_offset >= 64 AND byte_offset < 67502176),
    PRIMARY KEY(repository,blake3),
    FOREIGN KEY(repository,blake3) REFERENCES alt_blobs(repository,blake3) ON DELETE CASCADE,
    FOREIGN KEY(repository,segment_id) REFERENCES alt_segments(repository,segment_id)
);
CREATE INDEX alt_blob_locations_segment ON alt_blob_locations(repository,segment_id);
-- Git objects are stored as verified immutable pack segments: one row per
-- segment, object locations live in the segment's own index.
CREATE TABLE alt_pack_segments (
  repository text NOT NULL REFERENCES alt_repositories(id),
  segment_id text NOT NULL,
  sequence bigint NOT NULL CHECK (sequence > 0),
  objects bigint NOT NULL CHECK (objects > 0),
  bytes bigint NOT NULL CHECK (bytes > 0),
  flags integer NOT NULL,
  state text NOT NULL CHECK (state IN ('live','retired')),
  retired_at bigint,
  -- a small segment's whole file, made durable by the publishing commit itself;
  -- the file on disk is then only a cache until the segment is settled
  inline bytea,
  PRIMARY KEY(repository, segment_id)
);
CREATE INDEX alt_pack_segments_live ON alt_pack_segments(repository, sequence) WHERE state='live';
