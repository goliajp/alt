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
ALTER TABLE alt_repositories ADD COLUMN portable_import text;
