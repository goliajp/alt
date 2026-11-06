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
  PRIMARY KEY(repository,export_id)
);
-- A cutover freeze is independent of service entitlement and operator write
-- settings. Releasing it never turns either of those permissions on.
ALTER TABLE alt_repositories ADD COLUMN portable_freeze text;
