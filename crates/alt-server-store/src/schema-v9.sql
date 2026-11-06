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
